//! The names the assignments under a `begin`, loop or block write, which
//! their rescues, ensures, retries and later passes may see changed.
//!
//! One walk of the outermost body the checker asks about lists every
//! assignment in it, in order, and records the span of that list each
//! nested body covers. A nested body reuses the walk, and the distinct names
//! of a span are listed in time proportional to their number, so checking
//! bodies nested `n` deep never walks a descendant `n` times, and a name
//! written many times is listed once.

use crate::syntax::{Expr, Node, Statement, Stmt, Target, Try};
use std::collections::HashMap;

/// A span of the assignments one walk listed.
#[derive(Clone, Copy, Debug)]
pub(super) struct Span {
    root: u32,
    start: u32,
    end: u32,
}

/// The spans of a `begin`'s parts, which a walk lists in this order.
#[derive(Clone, Copy, Debug)]
pub(super) struct TrySpans {
    pub body: Span,
    pub rescues: Span,
    pub alternate: Span,
    pub ensure: Span,
    /// Whether a `retry` reruns the body: one in a rescue, outside any
    /// `begin` nested there that is rescuing its own error, and outside any
    /// block, where a `retry` would cross a call and fail.
    pub retry: bool,
}

impl TrySpans {
    /// What the body and the rescues assign, which a `retry` may follow.
    pub fn retried(&self) -> Span {
        Span {
            end: self.rescues.end,
            ..self.body
        }
    }

    /// What the body, the rescues and the `else` assign, which the ensure
    /// may follow.
    pub fn ensured(&self) -> Span {
        Span {
            end: self.alternate.end,
            ..self.body
        }
    }
}

/// One walk's assignments: where they start in the list, and the lowest
/// earlier position of each one's name over each run of them, as a tree.
struct Root {
    start: u32,
    width: u32,
    lowest: Vec<u32>,
}

/// A listing of a span's distinct names.
struct Query {
    span: Span,
    found: Vec<u32>,
}

#[derive(Default)]
pub(super) struct Assigns<'a> {
    /// Each distinct name, by id.
    names: Vec<&'a str>,
    ids: HashMap<&'a str, u32>,
    /// The name each assignment writes, in the order the walks met them.
    sites: Vec<u32>,
    /// For each assignment, one more than the position of the previous one
    /// of its name, or 0 for the first.
    previous: Vec<u32>,
    /// Each name's assignments, in order.
    positions: Vec<Vec<u32>>,
    roots: Vec<Root>,
    /// The span of each walked statement list, by its address and length.
    bodies: HashMap<(usize, usize), Span>,
    tries: HashMap<usize, TrySpans>,
    /// The `begin`s, by address, that a `retry` reruns.
    retried: std::collections::HashSet<usize>,
    /// What the lists of positions and the trees hold, beyond the storage
    /// of the tables that hold them.
    held: usize,
}

impl<'a> Assigns<'a> {
    /// The span of `body`'s assignments.
    pub fn body(&mut self, body: &'a [Stmt]) -> Span {
        if let Some(&span) = self.bodies.get(&key(body)) {
            return span;
        }
        self.walk(|walk| {
            walk.stmts(body);
        });
        self.bodies[&key(body)]
    }

    /// The spans of `attempt`'s parts.
    pub fn attempt(&mut self, attempt: &'a Try) -> TrySpans {
        let address = attempt as *const Try as usize;
        if let Some(&spans) = self.tries.get(&address) {
            return spans;
        }
        self.walk(|walk| walk.attempt(attempt));
        self.tries[&address]
    }

    /// The distinct names `span`'s assignments write, in the order of their
    /// first assignment there.
    pub fn distinct(&self, span: Span) -> Vec<&'a str> {
        let mut query = Query {
            span,
            found: Vec::new(),
        };
        if span.start < span.end {
            let root = &self.roots[span.root as usize];
            self.first(root, &mut query, 1, root.start, root.start + root.width);
        }
        query
            .found
            .into_iter()
            .map(|id| self.names[id as usize])
            .collect()
    }

    /// The bytes the lists, trees and maps hold.
    pub fn bytes(&self) -> usize {
        use super::meter::{map, set, vec};
        vec(&self.names)
            + map(&self.ids)
            + vec(&self.sites)
            + vec(&self.previous)
            + vec(&self.positions)
            + vec(&self.roots)
            + map(&self.bodies)
            + map(&self.tries)
            + set(&self.retried)
            + self.held
    }

    /// Whether some assignment in `span` writes `name`.
    pub fn writes(&self, span: Span, name: &str) -> bool {
        let Some(&id) = self.ids.get(name) else {
            return false;
        };
        let positions = &self.positions[id as usize];
        let at = positions.partition_point(|&position| position < span.start);
        positions
            .get(at)
            .is_some_and(|&position| position < span.end)
    }

    /// Lists the names of the assignments in `query`'s span, below `node` of
    /// `root`'s tree, which covers positions `left..right`. An assignment
    /// is its name's first in the span when the name's previous assignment
    /// comes before the span, so a subtree whose earliest previous
    /// assignment is in the span holds none.
    fn first(&self, root: &Root, query: &mut Query, node: usize, left: u32, right: u32) {
        let span = query.span;
        if right <= span.start || span.end <= left || root.lowest[node] > span.start {
            return;
        }
        if right - left == 1 {
            query.found.push(self.sites[left as usize]);
            return;
        }
        let middle = left + (right - left) / 2;
        self.first(root, query, 2 * node, left, middle);
        self.first(root, query, 2 * node + 1, middle, right);
    }

    /// Walks a body no earlier walk covered, as a new root.
    fn walk(&mut self, visit: impl FnOnce(&mut Walk<'a, '_>)) {
        let start = self.sites.len() as u32;
        let root = self.roots.len() as u32;
        let mut walk = Walk {
            assigns: self,
            root,
            target: None,
        };
        visit(&mut walk);
        let count = self.sites.len() - start as usize;
        let width = count.next_power_of_two().max(1);
        let mut lowest = vec![u32::MAX; 2 * width];
        lowest[width..width + count].copy_from_slice(&self.previous[start as usize..]);
        for node in (1..width).rev() {
            lowest[node] = lowest[2 * node].min(lowest[2 * node + 1]);
        }
        self.held += lowest.capacity() * std::mem::size_of::<u32>();
        self.roots.push(Root {
            start,
            width: width as u32,
            lowest,
        });
    }
}

fn key(body: &[Stmt]) -> (usize, usize) {
    (body.as_ptr() as usize, body.len())
}

/// One walk, which lists the assignments of every statement a body
/// contains, however deep in its expressions.
struct Walk<'a, 'w> {
    assigns: &'w mut Assigns<'a>,
    root: u32,
    /// The `begin` a `retry` here reruns, by address: the innermost one
    /// whose rescue the walk is in, as the runtime finds the innermost
    /// handler that is rescuing.
    target: Option<usize>,
}

impl<'a> Walk<'a, '_> {
    fn here(&self) -> u32 {
        self.assigns.sites.len() as u32
    }

    fn span(&self, start: u32) -> Span {
        Span {
            root: self.root,
            start,
            end: self.here(),
        }
    }

    fn site(&mut self, name: &'a str) {
        let assigns = &mut *self.assigns;
        let id = *assigns.ids.entry(name).or_insert_with(|| {
            assigns.names.push(name);
            assigns.positions.push(Vec::new());
            (assigns.names.len() - 1) as u32
        });
        let position = assigns.sites.len() as u32;
        let previous = assigns.positions[id as usize]
            .last()
            .map_or(0, |&last| last + 1);
        assigns.sites.push(id);
        assigns.previous.push(previous);
        let positions = &mut assigns.positions[id as usize];
        let before = positions.capacity();
        positions.push(position);
        assigns.held += (positions.capacity() - before) * std::mem::size_of::<u32>();
    }

    /// Lists `body`'s assignments and records its span.
    fn stmts(&mut self, body: &'a [Stmt]) -> Span {
        let start = self.here();
        for stmt in body {
            match &stmt.node {
                Statement::Assign(target, _, value) => {
                    self.target(target);
                    self.expr(value);
                }
                Statement::If(branches, alternate, _) => {
                    for (condition, body) in branches.iter() {
                        self.expr(condition);
                        self.stmts(body);
                    }
                    self.stmts(alternate);
                }
                Statement::While(condition, body, _) => {
                    self.expr(condition);
                    self.stmts(body);
                }
                Statement::For(target, iterable, body) => {
                    self.target(target);
                    self.expr(iterable);
                    self.stmts(body);
                }
                Statement::Expr(expr) => self.expr(expr),
                Statement::Return(Some(expr))
                | Statement::Break(Some(expr))
                | Statement::Next(Some(expr)) => self.expr(expr),
                Statement::Raise(value, message) => {
                    for expr in value.iter().chain(message) {
                        self.expr(expr);
                    }
                }
                Statement::Retry => {
                    if let Some(target) = self.target {
                        self.assigns.retried.insert(target);
                    }
                }
                _ => (),
            }
        }
        let span = self.span(start);
        self.assigns.bodies.insert(key(body), span);
        span
    }

    fn attempt(&mut self, attempt: &'a Try) {
        let address = attempt as *const Try as usize;
        // A `retry` in the body, the `else` or the ensure reruns an
        // enclosing `begin`, since this one is not rescuing then.
        let body = self.stmts(&attempt.body);
        let start = self.here();
        let outer = self.target.replace(address);
        for rescue in attempt.rescues.iter() {
            self.stmts(&rescue.body);
        }
        self.target = outer;
        let rescues = self.span(start);
        let alternate = self.stmts(&attempt.alternate);
        let ensure = self.stmts(&attempt.ensure);
        let spans = TrySpans {
            body,
            rescues,
            alternate,
            ensure,
            retry: self.assigns.retried.contains(&address),
        };
        self.assigns.tries.insert(address, spans);
    }

    fn target(&mut self, target: &'a Target) {
        match target {
            Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) => self.site(name),
            // An element or member target evaluates its receiver and
            // selectors, which may contain blocks.
            Target::Value(expr) => self.expr(expr),
            Target::Typed(inner, _) => self.target(inner),
            Target::Tuple(parts) => {
                for (part, _) in parts.iter() {
                    if let Some(part) = part {
                        self.target(part);
                    }
                }
            }
        }
    }

    /// Lists the assignments in every statement `expr` contains: blocks,
    /// `begin`s and compound statements anywhere inside it.
    fn expr(&mut self, expr: &'a Expr) {
        let mut pending = vec![expr];
        while let Some(expr) = pending.pop() {
            match &expr.node {
                Node::Compound(stmt) => {
                    self.stmts(std::slice::from_ref(&**stmt));
                }
                Node::Try(attempt) => self.attempt(attempt),
                Node::BlockCall(call, block) => {
                    pending.push(call);
                    // A block is a call, which a `retry` cannot leave.
                    let outer = self.target.take();
                    self.stmts(&block.body);
                    self.target = outer;
                }
                Node::Shape(_, Some(fallback), _) => pending.push(fallback),
                Node::Template(values, _) | Node::Array(values) | Node::Yield(values) => {
                    pending.extend(values.iter());
                }
                Node::Hash(entries) => pending.extend(entries.iter().map(|(_, v)| v)),
                Node::Unary(_, v) => pending.push(v),
                Node::Binary(_, l, r) => {
                    pending.push(l);
                    pending.push(r);
                }
                Node::Range(start, end, _) => {
                    pending.extend(start.as_deref());
                    pending.extend(end.as_deref());
                }
                Node::Conditional(branches, alternate) => {
                    for (c, v) in branches.iter() {
                        pending.push(c);
                        pending.push(v);
                    }
                    pending.push(alternate);
                }
                Node::Case(subject, whens, alternate) => {
                    pending.extend(subject.as_deref());
                    for when in whens.iter() {
                        pending.extend(when.values.iter().map(|(value, _)| value));
                        pending.push(&when.result);
                    }
                    pending.extend(alternate.as_deref());
                }
                Node::Call(_, args, _) => pending.extend(args.iter().map(|a| &a.value)),
                Node::ComputedCall(callee, args) => {
                    pending.push(callee);
                    pending.extend(args.iter().map(|a| &a.value));
                }
                Node::Member(recv, _) | Node::SafeMember(recv, _) => pending.push(recv),
                Node::Scope(recv, _, args) => {
                    pending.push(recv);
                    pending.extend(args.iter().flat_map(|args| args.iter().map(|a| &a.value)));
                }
                Node::Method(recv, _, args, _) | Node::SafeMethod(recv, _, args, _) => {
                    pending.push(recv);
                    pending.extend(args.iter().map(|a| &a.value));
                }
                Node::Index(recv, selectors) => {
                    pending.push(recv);
                    pending.extend(selectors.iter());
                }
                Node::Regex(..)
                | Node::Shape(_, None, _)
                | Node::Integer(_)
                | Node::BigInteger(..)
                | Node::Literal(_)
                | Node::Var(_) => (),
            }
        }
    }
}
