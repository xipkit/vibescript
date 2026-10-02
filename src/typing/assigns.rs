//! The names the assignments under a `begin`, loop or block write, which
//! their rescues, ensures, retries and later passes may see changed.
//!
//! One walk of the outermost body the checker asks about lists every
//! assignment in it, in order, and records the span of that list each
//! nested body covers. A nested body reuses the walk, and the distinct names
//! of a span are listed in time proportional to their number, so checking
//! bodies nested `n` deep never walks a descendant `n` times, and a name
//! written many times is listed once.
//!
//! A walk charges each statement and expression it visits to the check's
//! meter, and stops with the check.

use super::{
    counted::{CountedMap, CountedSet, CountedVec},
    meter::Meter,
};
use crate::syntax::{Argument, Expr, Node, Statement, Stmt, Target, Try, When};
use std::{iter::Rev, slice::Iter};

fn implicit_bit(name: &str) -> u16 {
    match name.as_bytes() {
        b"it" => 1,
        [b'_', digit @ b'1'..=b'9'] => 1 << (digit - b'0'),
        _ => 0,
    }
}

/// A span of the assignments one walk listed.
#[derive(Clone, Copy, Debug)]
pub(super) struct Span {
    root: u32,
    start: u32,
    end: u32,
    scope: u32,
    implicit_assignments: u16,
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
    lowest_scope: Vec<u32>,
}

impl super::counted::Owned for Root {
    fn owned(&self) -> usize {
        (self.lowest.capacity() + self.lowest_scope.capacity()) * std::mem::size_of::<u32>()
    }
}

impl super::counted::Owned for Span {
    fn owned(&self) -> usize {
        0
    }
}

impl super::counted::Owned for TrySpans {
    fn owned(&self) -> usize {
        0
    }
}

#[derive(Default)]
pub(super) struct Assigns<'a> {
    /// Each distinct name, by id.
    names: CountedVec<(&'a str, u32)>,
    ids: CountedMap<(&'a str, u32), u32>,
    /// Increasing scope ids let a span skip assignments to nested bindings.
    next_scope: u32,
    /// The name each assignment writes, in the order the walks met them.
    sites: CountedVec<u32>,
    /// For each assignment, one more than the position of the previous one
    /// of its name, or 0 for the first.
    previous: CountedVec<u32>,
    /// Each name's assignments, in order.
    positions: CountedVec<CountedVec<u32>>,
    roots: CountedVec<Root>,
    /// The span of each walked statement list, by its address and length.
    bodies: CountedMap<(usize, usize), Span>,
    tries: CountedMap<usize, TrySpans>,
    /// The `begin`s, by address, that a `retry` reruns.
    retried: CountedSet<usize>,
    /// What the lists of positions and the trees hold, beyond the storage
    /// of the tables that hold them.
    held: usize,
}

impl<'a> Assigns<'a> {
    /// The span of `body`'s assignments, charging the walk that lists them
    /// to `meter`. A walk the check stopped lists none.
    pub fn body(&mut self, meter: &Meter, body: &'a [Stmt]) -> Span {
        if let Some(&span) = self.bodies.get(&key(body)) {
            return span;
        }
        self.walk(meter, |walk| {
            walk.stmts(body);
        });
        self.bodies.get(&key(body)).copied().unwrap_or(Span::EMPTY)
    }

    /// The spans of `attempt`'s parts, charging the walk that lists them to
    /// `meter`. A walk the check stopped lists none.
    pub fn attempt(&mut self, meter: &Meter, attempt: &'a Try) -> TrySpans {
        let address = attempt as *const Try as usize;
        if let Some(&spans) = self.tries.get(&address) {
            return spans;
        }
        self.walk(meter, |walk| walk.attempt(attempt));
        self.tries.get(&address).copied().unwrap_or(TrySpans {
            body: Span::EMPTY,
            rescues: Span::EMPTY,
            alternate: Span::EMPTY,
            ensure: Span::EMPTY,
            retry: false,
        })
    }

    /// The distinct names `span`'s assignments write, in the order of their
    /// first assignment there. They are counted before they are listed:
    /// `reserve` is given their number and the bytes of the names, and
    /// none are listed unless it returns true.
    pub fn distinct(&self, span: Span, reserve: impl FnOnce(usize, usize) -> bool) -> Vec<&'a str> {
        if span.start >= span.end {
            return Vec::new();
        }
        let root = &self.roots[span.root as usize];
        let all = |found: &mut dyn FnMut(u32)| {
            self.first(root, span, 1, root.start, root.start + root.width, found);
        };
        let (mut count, mut bytes) = (0, 0);
        all(&mut |id| {
            count += 1;
            bytes += self.names[id as usize].0.len();
        });
        if !reserve(count, bytes) {
            return Vec::new();
        }
        let mut names = Vec::with_capacity(count);
        all(&mut |id| names.push(self.names[id as usize].0));
        names
    }

    /// The bytes the lists, trees and maps hold.
    pub fn bytes(&self) -> usize {
        use super::meter::{map, set, vec};
        vec(self.names.as_vec())
            + map(&self.ids)
            + vec(self.sites.as_vec())
            + vec(self.previous.as_vec())
            + vec(self.positions.as_vec())
            + vec(self.roots.as_vec())
            + map(&self.bodies)
            + map(&self.tries)
            + set(&self.retried)
            + self.held
    }

    /// Gives `found` the name of each assignment in `span` that is its
    /// name's first there, below `node` of `root`'s tree, which covers
    /// positions `left..right`. An assignment is its name's first in the
    /// span when the name's previous assignment comes before the span, so a
    /// subtree whose earliest previous assignment is in the span holds none.
    fn first(
        &self,
        root: &Root,
        span: Span,
        node: usize,
        left: u32,
        right: u32,
        found: &mut dyn FnMut(u32),
    ) {
        if right <= span.start
            || span.end <= left
            || root.lowest[node] > span.start
            || root.lowest_scope[node] > span.scope
        {
            return;
        }
        if right - left == 1 {
            found(self.sites[left as usize]);
            return;
        }
        let middle = left + (right - left) / 2;
        self.first(root, span, 2 * node, left, middle, found);
        self.first(root, span, 2 * node + 1, middle, right, found);
    }

    /// Walks a body no earlier walk covered, as a new root. A walk the
    /// budget refuses room for its root lists nothing.
    fn walk(&mut self, meter: &Meter, visit: impl FnOnce(&mut Walk<'a, '_>)) {
        // The spans the walk records name its root, which is counted
        // before they are.
        if self.roots.reserve(meter.tables(), 1).is_err() {
            return;
        }
        let start = self.sites.len() as u32;
        let root = self.roots.len() as u32;
        let mut walk = Walk {
            assigns: self,
            root,
            scope: 0,
            implicit_assignments: 0,
            bindings: CountedMap::new(),
            shadows: CountedVec::new(),
            target: None,
            pending: Pending::default(),
            meter,
            visited: 0,
            stopped: meter.stopped(),
        };
        visit(&mut walk);
        let stopped = walk.finish();
        drop(walk);
        let count = self.sites.len() - start as usize;
        let width = count.next_power_of_two().max(1);
        // The tree, twice as wide as the assignments rounded up to a power
        // of two, is counted before it is built.
        let tree = 4 * width * std::mem::size_of::<u32>();
        let kept = meter.tables().keep(tree);
        let (false, Ok(mut kept)) = (stopped, kept) else {
            // The spans the stopped walk recorded cover no positions of its
            // empty tree, so they find no assignments.
            self.roots.push_within(Root {
                start,
                width: 0,
                lowest: Vec::new(),
                lowest_scope: Vec::new(),
            });
            return;
        };
        let mut lowest = vec![u32::MAX; 2 * width];
        let mut lowest_scope = vec![u32::MAX; 2 * width];
        lowest[width..width + count].copy_from_slice(&self.previous[start as usize..]);
        for (place, &id) in self.sites[start as usize..].iter().enumerate() {
            lowest_scope[width + place] = self.names[id as usize].1;
        }
        for node in (1..width).rev() {
            lowest[node] = lowest[2 * node].min(lowest[2 * node + 1]);
            lowest_scope[node] = lowest_scope[2 * node].min(lowest_scope[2 * node + 1]);
        }
        self.held += (lowest.capacity() + lowest_scope.capacity()) * std::mem::size_of::<u32>();
        self.roots.push_kept(
            &mut kept,
            Root {
                start,
                width: width as u32,
                lowest,
                lowest_scope,
            },
        );
    }
}

impl Span {
    /// Whether the body assigns this implicit parameter outside nested
    /// blocks, which prevents the compiler from binding it as a parameter.
    pub fn assigns_implicit(self, name: &str) -> bool {
        self.implicit_assignments & implicit_bit(name) != 0
    }

    /// A span with no assignments.
    const EMPTY: Self = Self {
        root: 0,
        start: 0,
        end: 0,
        scope: 0,
        implicit_assignments: 0,
    };
}

fn key(body: &[Stmt]) -> (usize, usize) {
    (body.as_ptr() as usize, body.len())
}

/// A walk charges the meter and checks the budget once per this many
/// statements and expressions.
const PACE: u64 = 64;

/// The most entries the stack of expressions to visit takes for one: what
/// an expression's children add, three at most, and what taking the next
/// one adds, one at most.
const ROOM: usize = 4;

/// One walk, which lists the assignments of every statement a body
/// contains, however deep in its expressions.
struct Walk<'a, 'w> {
    assigns: &'w mut Assigns<'a>,
    root: u32,
    scope: u32,
    bindings: CountedMap<&'a str, u32>,
    implicit_assignments: u16,
    shadows: CountedVec<(&'a str, Option<u32>)>,
    /// The `begin` a `retry` here reruns, by address: the innermost one
    /// whose rescue the walk is in, as the runtime finds the innermost
    /// handler that is rescuing.
    target: Option<usize>,
    /// The expressions the walk has yet to visit.
    pending: Pending<'a>,
    meter: &'w Meter,
    /// The statements and expressions visited since the meter was last
    /// charged.
    visited: u64,
    /// Whether the check has stopped, so the walk lists no more.
    stopped: bool,
}

impl<'a> Walk<'a, '_> {
    /// Makes room on the stack of expressions to visit for what an
    /// expression and the next one taken add, the moment it grows checked
    /// against the budget first. Returns whether the walk should stop.
    fn room(&mut self) -> bool {
        if !self.stopped && super::walk::room(&mut self.pending.stack, ROOM, self.meter, 0) {
            self.stopped = true;
        }
        self.stopped
    }

    /// Counts a statement or an expression, charging the meter for each
    /// [`PACE`] of them. Returns whether the walk should stop.
    fn visit(&mut self) -> bool {
        self.visited += 1;
        if self.visited == PACE {
            self.visited = 0;
            // What the tables grew by is counted as they grow.
            self.stopped = self.meter.pace(PACE, self.pending.bytes()) || self.stopped;
        }
        self.stopped
    }

    /// Charges the statements and expressions visited since the meter was
    /// last charged. Returns whether the check has stopped.
    fn finish(&mut self) -> bool {
        let visited = std::mem::take(&mut self.visited);
        self.stopped = self.meter.pace(visited, self.pending.bytes()) || self.stopped;
        self.stopped
    }

    fn here(&self) -> u32 {
        self.assigns.sites.len() as u32
    }

    fn span(&self, start: u32) -> Span {
        Span {
            root: self.root,
            start,
            end: self.here(),
            scope: self.scope,
            implicit_assignments: self.implicit_assignments,
        }
    }

    /// Lists an assignment to `name`. It, and a new name's entries, are
    /// counted in every list they go in before any changes; a refusal
    /// lists nothing, and the walk stops.
    fn site(&mut self, name: &'a str) {
        if self.meter.charge(1 + (name.len() / 64) as u64) {
            self.stopped = true;
            return;
        }
        let scope = self.bindings.get(name).copied().unwrap_or(0);
        self.implicit_assignments |= implicit_bit(name);
        let tables = self.meter.tables();
        let assigns = &mut *self.assigns;
        let known = assigns.ids.get(&(name, scope)).copied();
        let mut fresh = CountedVec::new();
        let before = known.map_or(0, |id| assigns.positions[id as usize].capacity());
        let room = assigns.sites.reserve(tables, 1).is_ok()
            && assigns.previous.reserve(tables, 1).is_ok()
            && match known {
                Some(id) => assigns.positions[id as usize].reserve(tables, 1).is_ok(),
                None => {
                    fresh.reserve(tables, 1).is_ok()
                        && assigns.names.reserve(tables, 1).is_ok()
                        && assigns.positions.reserve(tables, 1).is_ok()
                        && assigns.ids.reserve(tables, 1).is_ok()
                }
            };
        if !room {
            self.stopped = true;
            return;
        }
        let id = known.unwrap_or_else(|| {
            let id = assigns.names.len() as u32;
            assigns.names.push_within((name, scope));
            assigns.positions.push_within(fresh);
            assigns.ids.insert_within((name, scope), id);
            id
        });
        let position = assigns.sites.len() as u32;
        let positions = &mut assigns.positions[id as usize];
        let previous = positions.last().map_or(0, |&last| last + 1);
        positions.push_within(position);
        assigns.held += (positions.capacity() - before) * std::mem::size_of::<u32>();
        assigns.sites.push_within(id);
        assigns.previous.push_within(previous);
    }

    fn scope(&mut self) -> u32 {
        self.assigns.next_scope += 1;
        self.assigns.next_scope
    }

    fn shadow(&mut self, scope: u32, name: &'a str) {
        if self.meter.charge(1 + (name.len() / 64) as u64) {
            self.stopped = true;
            return;
        }
        let previous = self.bindings.get(name).copied();
        let ledger = self.meter.scratch_lists();
        if self.shadows.push(ledger, (name, previous)).is_err()
            || self.bindings.insert(ledger, name, scope).is_err()
        {
            self.stopped = true;
        }
    }

    fn restore(&mut self, mark: usize) {
        while self.shadows.len() > mark {
            if self.stopped || self.meter.stopped() {
                self.shadows.truncate(mark);
                return;
            }
            let (name, previous) = self.shadows.pop().unwrap();
            if self.meter.charge(1 + (name.len() / 64) as u64) {
                self.stopped = true;
                continue;
            }
            match previous {
                Some(scope) => {
                    if self
                        .bindings
                        .insert(self.meter.scratch_lists(), name, scope)
                        .is_err()
                    {
                        self.stopped = true;
                    }
                }
                None => {
                    self.bindings.remove(name);
                }
            }
        }
    }

    fn bind(&mut self, scope: u32, target: &'a Target) {
        if self.visit() {
            return;
        }
        match target {
            Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) => {
                self.shadow(scope, name.as_str());
            }
            Target::Typed(target, _) => self.bind(scope, target),
            Target::Tuple(parts) => {
                for (target, _) in parts.iter() {
                    if self.stopped {
                        break;
                    }
                    if let Some(target) = target {
                        self.bind(scope, target);
                    }
                }
            }
            _ => (),
        }
    }

    /// Lists `body`'s assignments and records its span, unless the check
    /// stops first.
    fn stmts(&mut self, body: &'a [Stmt]) -> Span {
        let start = self.here();
        for stmt in body {
            if self.visit() {
                return self.span(start);
            }
            // A statement too tall to check on this platform is refused
            // where the checker reaches it, and is not entered here, where
            // the walk recurses once for each statement nested in another.
            if super::too_tall(stmt.height()) {
                continue;
            }
            match &stmt.node {
                Statement::Assign(target, _, value) => {
                    self.target(target);
                    self.expr(value);
                }
                Statement::If(branches, alternate, _) => {
                    for (condition, body) in branches.iter() {
                        // A walk a branch stops lists no more of them.
                        if self.stopped {
                            break;
                        }
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
                        let tables = self.meter.tables();
                        if self.assigns.retried.insert(tables, target).is_err() {
                            self.stopped = true;
                        }
                    }
                }
                _ => (),
            }
        }
        let span = self.span(start);
        if !self.stopped {
            let tables = self.meter.tables();
            if self.assigns.bodies.insert(tables, key(body), span).is_err() {
                self.stopped = true;
            }
        }
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
            if self.stopped {
                break;
            }
            let scope = self.scope();
            let shadows = self.shadows.len();
            if let Some(name) = &rescue.binding {
                self.shadow(scope, name.as_str());
            }
            let outer_scope = std::mem::replace(&mut self.scope, scope);
            self.stmts(&rescue.body);
            self.scope = outer_scope;
            self.restore(shadows);
        }
        self.target = outer;
        let rescues = self.span(start);
        let alternate = self.stmts(&attempt.alternate);
        let ensure = self.stmts(&attempt.ensure);
        if self.stopped {
            return;
        }
        let spans = TrySpans {
            body,
            rescues,
            alternate,
            ensure,
            retry: self.assigns.retried.contains(&address),
        };
        let tables = self.meter.tables();
        if self.assigns.tries.insert(tables, address, spans).is_err() {
            self.stopped = true;
        }
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
                // Each part is a visit, and a walk a part stops lists no
                // more of them.
                for (part, _) in parts.iter() {
                    if self.visit() {
                        return;
                    }
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
        let base = self.pending.len();
        if self.room() {
            return;
        }
        self.pending.push(expr);
        while let Some(expr) = self.pending.pop(base) {
            // An expression adds at most a few entries, and taking one adds
            // at most one more, room for which is made first.
            if self.visit() || self.room() {
                self.pending.truncate(base);
                return;
            }
            self.pending.children(expr);
            match &expr.node {
                Node::Compound(stmt) => {
                    self.stmts(std::slice::from_ref(&**stmt));
                }
                Node::Try(attempt) => self.attempt(attempt),
                Node::BlockCall(_, block) => {
                    // A block is a call, which a `retry` cannot leave.
                    let outer = self.target.take();
                    let outer_implicit = std::mem::take(&mut self.implicit_assignments);
                    let scope = self.scope();
                    let shadows = self.shadows.len();
                    if !block.implicit {
                        for target in &block.params {
                            self.bind(scope, target);
                        }
                    }
                    let outer_scope = std::mem::replace(&mut self.scope, scope);
                    self.stmts(&block.body);
                    self.implicit_assignments = outer_implicit;
                    self.scope = outer_scope;
                    self.restore(shadows);
                    self.target = outer;
                }
                _ => (),
            }
        }
    }
}

impl Drop for Walk<'_, '_> {
    fn drop(&mut self) {
        self.meter
            .dropped(super::meter::map(&self.bindings) + super::meter::vec(self.shadows.as_vec()));
    }
}

/// The expressions a walk over the syntax has yet to visit: single ones,
/// and what is left of lists, which it takes one at a time, so a wide list
/// is never copied. A walk visits an expression before its children, and
/// the children last to first, finishing each before the next.
#[derive(Default)]
struct Pending<'a> {
    stack: Vec<Next<'a>>,
}

enum Next<'a> {
    Expr(&'a Expr),
    Exprs(Rev<Iter<'a, Expr>>),
    Arguments(Rev<Iter<'a, Argument>>),
    Pairs(Rev<Iter<'a, (crate::compilation::Bytes, Expr)>>),
    /// A conditional's conditions and results, each result before its
    /// condition.
    Branches(Rev<Iter<'a, (Expr, Expr)>>),
    /// A `case`'s `when`s, each result before its values.
    Whens(Rev<Iter<'a, When>>),
    Values(Rev<Iter<'a, (Expr, bool)>>),
}

impl<'a> Pending<'a> {
    /// The number of entries, which [`Self::pop`] takes as a floor.
    fn len(&self) -> usize {
        self.stack.len()
    }

    /// The bytes the entries take.
    fn bytes(&self) -> usize {
        self.stack.capacity() * std::mem::size_of::<Next<'_>>()
    }

    /// Drops the entries above the first `len`.
    fn truncate(&mut self, len: usize) {
        self.stack.truncate(len);
    }

    fn push(&mut self, expr: &'a Expr) {
        self.stack.push(Next::Expr(expr));
    }

    /// Adds `expr`'s children other than the statements it holds: those of
    /// a `begin`, a block or a compound statement, which the walk visits
    /// itself.
    fn children(&mut self, expr: &'a Expr) {
        let stack = &mut self.stack;
        let mut list = |next: Next<'a>, empty: bool| {
            if !empty {
                stack.push(next);
            }
        };
        match &expr.node {
            Node::BlockCall(call, _) => list(Next::Expr(call), false),
            Node::Shape(_, Some(fallback), _) => list(Next::Expr(fallback), false),
            Node::Template(values, _) | Node::Array(values) | Node::Yield(values) => {
                list(Next::Exprs(values.iter().rev()), values.is_empty());
            }
            Node::Hash(entries) => list(Next::Pairs(entries.iter().rev()), entries.is_empty()),
            Node::Unary(_, value) => list(Next::Expr(value), false),
            Node::Binary(_, left, right) => {
                list(Next::Expr(left), false);
                list(Next::Expr(right), false);
            }
            Node::Range(start, end, _) => {
                if let Some(start) = start {
                    list(Next::Expr(start), false);
                }
                if let Some(end) = end {
                    list(Next::Expr(end), false);
                }
            }
            Node::Conditional(branches, alternate) => {
                list(Next::Branches(branches.iter().rev()), branches.is_empty());
                list(Next::Expr(alternate), false);
            }
            Node::Case(subject, whens, alternate) => {
                if let Some(subject) = subject {
                    list(Next::Expr(subject), false);
                }
                list(Next::Whens(whens.iter().rev()), whens.is_empty());
                if let Some(alternate) = alternate {
                    list(Next::Expr(alternate), false);
                }
            }
            Node::Call(_, args, _) => list(Next::Arguments(args.iter().rev()), args.is_empty()),
            Node::ComputedCall(receiver, args)
            | Node::Method(receiver, _, args, _)
            | Node::SafeMethod(receiver, _, args, _) => {
                list(Next::Expr(receiver), false);
                list(Next::Arguments(args.iter().rev()), args.is_empty());
            }
            Node::Scope(receiver, _, args) => {
                list(Next::Expr(receiver), false);
                if let Some(args) = args {
                    list(Next::Arguments(args.iter().rev()), args.is_empty());
                }
            }
            Node::Member(receiver, _) | Node::SafeMember(receiver, _) => {
                list(Next::Expr(receiver), false);
            }
            Node::Index(receiver, selectors) => {
                list(Next::Expr(receiver), false);
                list(Next::Exprs(selectors.iter().rev()), selectors.is_empty());
            }
            Node::Try(_)
            | Node::Compound(_)
            | Node::Regex(..)
            | Node::Shape(_, None, _)
            | Node::Integer(_)
            | Node::BigInteger(..)
            | Node::Literal(_)
            | Node::Var(_) => (),
        }
    }

    /// Takes the next expression to visit, while more than `floor` entries
    /// remain.
    fn pop(&mut self, floor: usize) -> Option<&'a Expr> {
        if self.stack.len() <= floor {
            return None;
        }
        let top = self.stack.last_mut()?;
        let (found, then) = match top {
            Next::Expr(expr) => (*expr, None),
            Next::Exprs(items) => (items.next()?, None),
            Next::Arguments(items) => (&items.next()?.value, None),
            Next::Pairs(items) => (&items.next()?.1, None),
            Next::Values(items) => (&items.next()?.0, None),
            Next::Branches(items) => {
                let (condition, result) = items.next()?;
                (result, Some(Next::Expr(condition)))
            }
            Next::Whens(items) => {
                let when = items.next()?;
                let values =
                    (!when.values.is_empty()).then(|| Next::Values(when.values.iter().rev()));
                (&when.result, values)
            }
        };
        if top.exhausted() {
            self.stack.pop();
        }
        self.stack.extend(then);
        Some(found)
    }
}

impl Next<'_> {
    /// Whether nothing is left to take.
    fn exhausted(&self) -> bool {
        match self {
            // A single expression is taken whole.
            Next::Expr(_) => true,
            Next::Exprs(items) => items.len() == 0,
            Next::Arguments(items) => items.len() == 0,
            Next::Pairs(items) => items.len() == 0,
            Next::Branches(items) => items.len() == 0,
            Next::Whens(items) => items.len() == 0,
            Next::Values(items) => items.len() == 0,
        }
    }
}
