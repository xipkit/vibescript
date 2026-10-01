//! Source spans of syntax nodes, which record only where they start: the
//! parser's tokens give each node's end.

use super::{
    counted::{CountedMap, Refused},
    walk::{Item, Next, Walk},
};
use crate::{
    diagnostic::Span,
    syntax::{CallForm, Expr, Node, Statement, Stmt},
    tooling::{Token, TokenKind},
};

/// What follows a node's rightmost child in the source, which the tree
/// does not locate.
enum Trail<'e> {
    /// Nothing: the child ends the node.
    None,
    /// A member name after `.`, `&.` or `::`, and whether `()` follows it.
    Member(&'e str, bool),
}

pub(crate) struct Spans<'a> {
    source: &'a str,
    tokens: std::borrow::Cow<'a, [Token]>,
    /// The check's account, which tokens and nodes visited are charged to.
    meter: std::sync::Arc<super::meter::Meter>,
    /// Where each expression's last token starts, by node, so shared
    /// subtrees are walked once.
    lasts: std::cell::RefCell<CountedMap<usize, usize>>,
    /// [`Self::furthest`] of each statement measured, by node, so a
    /// statement nested in many that are measured is walked once.
    furthest: std::cell::RefCell<CountedMap<usize, usize>>,
    /// What the tokens hold when they are a merged copy of their own.
    owned: usize,
    /// The most that parsing one interpolation again held.
    parsing: usize,
}

impl<'a> Spans<'a> {
    pub fn new(
        source: &'a str,
        tokens: &'a [Token],
        interpolations: &[(u32, u32)],
        meter: std::sync::Arc<super::meter::Meter>,
    ) -> Self {
        let mut tokens = std::borrow::Cow::Borrowed(tokens);
        // The parser lists each interpolation separately from its outer string
        // token. Merge their tokens by source offset for member and edit spans.
        // Each is parsed again for its tokens, which holds its syntax for a
        // moment, within the steps and memory the check may spend, and its
        // steps are the check's; one that runs out of them stops the check.
        let size = std::mem::size_of::<Token>();
        // Holds `bytes` in the account until the checker measures its own
        // tables, stopping the check if they pass the memory left, before
        // what they count is made. Returns whether the check has stopped.
        let hold = |bytes: usize| {
            meter.outside(bytes);
            let held = meter.held(0);
            if meter.budget().memory.is_some_and(|left| held > left) {
                meter.stop();
            }
            meter.stopped()
        };
        let mut parsing = 0;
        // What the merged copy's tokens hold beyond the list, once it is
        // made.
        let mut payloads = 0;
        for &(start, end) in interpolations {
            if meter.stopped() {
                break;
            }
            let start = start as usize;
            let end = end as usize - 1;
            let budget = meter.budget();
            let merged = match &tokens {
                std::borrow::Cow::Borrowed(_) => 0,
                std::borrow::Cow::Owned(list) => list.capacity() * size + payloads,
            };
            let mut context = crate::CallContext::new(crate::CallOptions {
                limits: crate::Limits {
                    steps: budget.steps.map(|left| left.saturating_sub(meter.steps())),
                    memory_bytes: budget.memory.map(|left| left.saturating_sub(merged)),
                    ..crate::Limits::default()
                },
                cancellation: budget.cancellation.clone().unwrap_or_default(),
                deadline: budget.deadline,
                ..crate::CallOptions::default()
            });
            let inner = crate::syntax::record::tokens_within(
                &source[start..end],
                &crate::compilation::Meter(std::cell::RefCell::new(&mut context)),
            );
            let used = context.stats();
            let charged = meter.charge(used.steps);
            parsing = parsing.max(used.peak_memory_bytes);
            // A check the parse's work stops merges no more tokens.
            if meter.scratch(merged + used.peak_memory_bytes) || charged {
                break;
            }
            match inner {
                Ok(inner) => {
                    // Appending copies the parser's tokens the first time,
                    // and growing the copy holds its old buffer beside the
                    // new one for a moment: both are counted before they
                    // are made.
                    let (length, before, copied) = match &tokens {
                        std::borrow::Cow::Borrowed(list) => {
                            (list.len(), 0, super::meter::Heap::heap(*list))
                        }
                        std::borrow::Cow::Owned(list) => (list.len(), list.capacity(), payloads),
                    };
                    let added = super::meter::Heap::heap(inner.as_slice());
                    let grown = 2 * (length + inner.len());
                    if hold((before + grown) * size + copied + added) {
                        break;
                    }
                    payloads = copied + added;
                    tokens
                        .to_mut()
                        .extend(inner.into_iter().filter_map(|mut token| {
                            if token.kind == TokenKind::Eof {
                                return None;
                            }
                            token.span = token.span.start + start..token.span.end + start;
                            Some(token)
                        }));
                }
                Err(error)
                    if matches!(
                        error.kind,
                        crate::ErrorKind::Steps
                            | crate::ErrorKind::Memory
                            | crate::ErrorKind::Deadline
                            | crate::ErrorKind::Cancelled
                    ) =>
                {
                    meter.stop();
                }
                Err(_) => (),
            }
        }
        let mut owned = 0;
        if let std::borrow::Cow::Owned(tokens) = &mut tokens {
            owned = super::meter::Heap::heap(tokens);
            // Sorting them in order keeps a copy of them for a moment, which
            // is counted first; a check that it stops never reads them.
            if !hold(owned + tokens.len() * size)
                && super::counted::sort_by(&meter, tokens, |a, b| a.span.start.cmp(&b.span.start))
                    .is_err()
            {
                // The budget stopped the check, which never reads them.
                debug_assert!(meter.stopped());
            }
        }
        Self {
            source,
            tokens,
            meter,
            lasts: std::cell::RefCell::default(),
            furthest: std::cell::RefCell::default(),
            owned,
            parsing,
        }
    }

    /// The most that parsing one interpolation again held, as the pass
    /// over the canonical surface does too.
    pub fn parsing(&self) -> usize {
        self.parsing
    }

    /// What the spans' tables hold.
    pub fn bytes(&self) -> usize {
        self.owned
            + super::meter::map(&self.lasts.borrow())
            + super::meter::map(&self.furthest.borrow())
    }

    /// What the larger of the tables of offsets holds, each of which grows
    /// by an entry a node.
    pub fn table(&self) -> usize {
        super::meter::map(&self.lasts.borrow()).max(super::meter::map(&self.furthest.borrow()))
    }

    /// Charges `count` steps of a scan over the tokens. Returns whether the
    /// check has stopped, when a scan gives up with the span it has.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    fn step(&self, count: usize) -> bool {
        self.meter.charge(count as u64)
    }

    /// The index of the token that starts at `offset`.
    fn token_at(&self, offset: usize) -> Option<usize> {
        self.tokens
            .binary_search_by_key(&offset, |token| token.span.start)
            .ok()
    }

    /// The index of the first token starting at or after `offset`.
    fn token_from(&self, offset: usize) -> usize {
        self.tokens
            .partition_point(|token| token.span.start < offset)
    }

    fn text(&self, index: usize) -> &'a str {
        let span = &self.tokens[index].span;
        &self.source[span.start..span.end]
    }

    /// The span of the word or token starting at `offset`.
    pub fn token(&self, offset: usize) -> Span {
        match self.token_at(offset) {
            Some(index) => {
                let span = &self.tokens[index].span;
                Span::new(span.start, span.end)
            }
            None => Span::new(offset, self.word_end(offset)),
        }
    }

    fn word_end(&self, offset: usize) -> usize {
        let bytes = self.source.as_bytes();
        let mut end = offset;
        while end < bytes.len()
            && (bytes[end].is_ascii_alphanumeric()
                || matches!(bytes[end], b'_' | b'@' | b'?' | b'!')
                || bytes[end] >= 0x80)
        {
            end += 1;
        }
        if end == offset {
            (offset + 1).min(bytes.len()).max(offset)
        } else {
            end
        }
    }

    /// The span of the first word `name` at or after `offset`, or of the
    /// token at `offset` when there is none.
    pub fn word_after(&self, offset: usize, name: &str) -> Span {
        let start = self.token_from(offset);
        for index in start..self.tokens.len().min(start + 256) {
            if self.step(1) {
                break;
            }
            if self.tokens[index].kind == TokenKind::Word && self.text(index) == name {
                let span = &self.tokens[index].span;
                return Span::new(span.start, span.end);
            }
        }
        self.token(offset)
    }

    /// The span of an expression, from its first token to its last,
    /// including the parentheses of a group it starts with.
    pub fn expr(&self, expr: &Expr) -> Span {
        let start = first_offset(expr);
        let last = self.last(expr);
        let end = self.close(start, last);
        Span::new(self.open(start, end), end)
    }

    /// The span of a statement.
    pub fn stmt(&self, stmt: &Stmt) -> Span {
        let start = stmt.offset as usize;
        let value = match &stmt.node {
            Statement::Expr(value) | Statement::Assign(_, _, value) => Some(value),
            Statement::Return(value) | Statement::Break(value) | Statement::Next(value) => {
                value.as_ref()
            }
            _ => None,
        };
        let furthest = self.stmt_furthest(stmt);
        let last = match value {
            Some(value) => self.last(value).max(furthest),
            None => furthest,
        };
        let last = last.max(start);
        Span::new(start, self.close(start, last))
    }

    /// Moves `start` back over each `(` that opens a group closing before
    /// `end`, as `(a + b)` does in `(a + b) * c`, whose tree starts at `a`.
    fn open(&self, mut start: usize, end: usize) -> usize {
        let Some(mut first) = self.token_at(start) else {
            return start;
        };
        while first > 0 && self.tokens[first - 1].kind == TokenKind::Punct('(') {
            // The matching `)` must come before the expression's end.
            let mut depth = 0;
            let mut closes = None;
            for index in first - 1..self.tokens.len() {
                if self.step(1) {
                    return start;
                }
                let token = &self.tokens[index];
                if token.span.start >= end {
                    break;
                }
                match token.kind {
                    TokenKind::Punct('(' | '[' | '{') => depth += 1,
                    TokenKind::Punct(')' | ']' | '}') => {
                        depth -= 1;
                        if depth == 0 {
                            closes = Some(token.span.end);
                            break;
                        }
                    }
                    _ => (),
                }
            }
            if closes.is_none_or(|close| close >= end) {
                break;
            }
            first -= 1;
            start = self.tokens[first].span.start;
        }
        start
    }

    /// The end of a node that starts at `start` and whose last child starts
    /// at `last`: the end of that token, then the closing brackets opened
    /// since `start`.
    fn close(&self, start: usize, last: usize) -> usize {
        let Some(last_index) = self.token_at(last) else {
            return self.word_end(last);
        };
        let first = self.token_from(start);
        let mut depth: i64 = 0;
        if self.step(last_index.saturating_sub(first) + 1) {
            return self.tokens[last_index].span.end;
        }
        for index in first..=last_index {
            match &self.tokens[index].kind {
                TokenKind::Punct('(' | '[' | '{') => depth += 1,
                TokenKind::Punct(')' | ']' | '}') => depth -= 1,
                _ => (),
            }
        }
        let mut end = self.tokens[last_index].span.end;
        let mut index = last_index + 1;
        while depth > 0 && index < self.tokens.len() {
            if self.step(1) {
                break;
            }
            match &self.tokens[index].kind {
                TokenKind::Punct(')' | ']' | '}') => {
                    depth -= 1;
                    end = self.tokens[index].span.end;
                }
                TokenKind::Punct('(' | '[' | '{') => depth += 1,
                TokenKind::Newline | TokenKind::Punct(',') => (),
                TokenKind::Word if self.text(index) == "end" => (),
                TokenKind::Eof => break,
                _ => (),
            }
            index += 1;
        }
        end
    }

    /// The span of the member name in a call on a receiver, such as
    /// `include?` in `items.include?(x)`: the first `.` or `&.` after the
    /// receiver that is followed by `name`.
    pub fn member(&self, receiver: &Expr, name: &str) -> Option<Span> {
        let last = self.last(receiver);
        let from = match self.token_at(last) {
            Some(index) => index,
            None => self.token_from(last),
        };
        let mut index = from;
        let limit = self.tokens.len().min(from + 512);
        while index + 1 < limit {
            if self.step(1) {
                return None;
            }
            let dot = matches!(
                self.tokens[index].kind,
                TokenKind::Punct('.') | TokenKind::Operator("&." | "::")
            );
            if dot {
                let mut next = index + 1;
                while next < limit && self.tokens[next].kind == TokenKind::Newline {
                    next += 1;
                }
                if next < limit && self.text(next) == name {
                    let span = &self.tokens[next].span;
                    return Some(Span::new(span.start, span.end));
                }
            }
            index += 1;
        }
        None
    }

    /// The dot or scope operator before a member's name.
    pub fn member_operator(&self, receiver: &Expr, name: &str) -> Option<Span> {
        let member = self.member(receiver, name)?;
        let mut index = self.token_from(member.start);
        while index > 0 {
            index -= 1;
            if self.step(1) {
                return None;
            }
            let token = &self.tokens[index];
            if token.kind != TokenKind::Newline {
                return Some(Span::new(token.span.start, token.span.end));
            }
        }
        None
    }

    /// The span of the `[`...`]` index selectors after a receiver, from the
    /// `[` to the `]`.
    pub fn index_brackets(&self, receiver: &Expr, whole: &Expr) -> Option<Span> {
        let end = self.expr(whole).end;
        let last = self.last(receiver);
        let from = self.token_at(last).unwrap_or_else(|| self.token_from(last));
        for index in from + 1..self.tokens.len().min(from + 64) {
            if self.tokens[index].kind == TokenKind::Punct('[') {
                return Some(Span::new(self.tokens[index].span.start, end));
            }
        }
        None
    }

    /// The start of an expression's last token, remembered per node so a
    /// chain of calls costs linear work. The tree locates every child but
    /// not a member's name, so the path of rightmost children is followed
    /// down to one the tree locates, and each member name on it is found
    /// in the tokens after its receiver.
    fn last(&self, expr: &Expr) -> usize {
        let mut path: Vec<(&Expr, Trail<'_>)> = Vec::new();
        let mut current = expr;
        let mut position = loop {
            let key = std::ptr::from_ref(current) as usize;
            if let Some(&known) = self.lasts.borrow().get(&key) {
                break known;
            }
            let (next, trail) = match &current.node {
                Node::Member(receiver, name)
                | Node::SafeMember(receiver, name)
                | Node::Scope(receiver, name, None) => {
                    (&**receiver, Trail::Member(name.as_str(), false))
                }
                Node::Scope(receiver, name, Some(args)) if args.is_empty() => {
                    (&**receiver, Trail::Member(name.as_str(), true))
                }
                Node::Method(receiver, name, args, form)
                | Node::SafeMethod(receiver, name, args, form)
                    if args.is_empty() =>
                {
                    let parenthesized = matches!(form, CallForm::Parenthesized);
                    (&**receiver, Trail::Member(name.as_str(), parenthesized))
                }
                // A command call ends with its last argument.
                Node::Method(_, _, args, CallForm::Bare | CallForm::Auto)
                | Node::SafeMethod(_, _, args, CallForm::Bare | CallForm::Auto)
                | Node::Call(_, args, CallForm::Bare | CallForm::Auto)
                    if !args.is_empty() =>
                {
                    (&args[args.len() - 1].value, Trail::None)
                }
                Node::Binary(_, _, right) => (&**right, Trail::None),
                Node::Unary(_, value) => (&**value, Trail::None),
                Node::Range(_, Some(end), _) => (&**end, Trail::None),
                _ => break self.furthest(Item::Expr(current), true),
            };
            path.push((current, trail));
            current = next;
        };
        let key = std::ptr::from_ref(current) as usize;
        self.remember(&self.lasts, key, position);
        for (node, trail) in path.into_iter().rev() {
            if let Trail::Member(name, parenthesized) = trail {
                position = self.name_after(position, name, parenthesized);
            }
            let key = std::ptr::from_ref(node) as usize;
            self.remember(&self.lasts, key, position);
        }
        position
    }

    /// Remembers `position` for node `key` in `table`, with its room
    /// counted before it is kept. One the budget refuses room for is not
    /// remembered: the check has stopped, and finds it again if asked.
    fn remember(
        &self,
        table: &std::cell::RefCell<CountedMap<usize, usize>>,
        key: usize,
        position: usize,
    ) {
        match table
            .borrow_mut()
            .insert(self.meter.tables(), key, position)
        {
            Ok(_) | Err(Refused) => (),
        }
    }

    /// The greatest start of a statement, expression, rescue or block in
    /// `root`, which is where its last token-bearing part starts, charging
    /// the walk that finds it. With `memo`, an expression other than `root`
    /// that holds no statement between them takes the start of its last
    /// token remembered in [`Self::lasts`], if any, instead of its parts.
    fn furthest(&self, root: Item<'_>, memo: bool) -> usize {
        let mut last = match root {
            Item::Stmt(stmt) => stmt.offset as usize,
            Item::Expr(expr) => expr.offset as usize,
            Item::Target(_) => 0,
        };
        let lasts = self.lasts.borrow();
        // Each entry carries whether its expressions may take a remembered
        // start: not below a statement, as a statement's parts are measured
        // afresh.
        let mut walk = Walk::new(&self.meter);
        walk.push(Next::Item(root), memo);
        let mut first = true;
        while let Some((item, remembered)) = walk.next(0) {
            let own = match item {
                // A statement below the root is measured once, however many
                // statements around it are.
                Item::Stmt(stmt) if !first => {
                    last = last.max(self.stmt_furthest(stmt));
                    continue;
                }
                Item::Stmt(stmt) => stmt.offset as usize,
                Item::Expr(expr) => expr.offset as usize,
                Item::Target(_) => 0,
            };
            last = last.max(own);
            if let Item::Expr(expr) = item {
                let key = std::ptr::from_ref(expr) as usize;
                if remembered && !first {
                    if let Some(&known) = lasts.get(&key) {
                        last = last.max(known);
                        continue;
                    }
                }
                match &expr.node {
                    Node::Try(attempt) => {
                        for rescue in attempt.rescues.iter() {
                            last = last.max(rescue.offset as usize);
                        }
                    }
                    Node::BlockCall(_, block) => {
                        last = last.max(block.offset as usize);
                        walk.push(Next::Targets(block.params.iter()), false);
                    }
                    _ => (),
                }
            }
            first = false;
            let below = remembered && matches!(item, Item::Expr(_));
            walk.children(item, below);
        }
        last
    }

    /// [`Self::furthest`] of a statement, measured once.
    fn stmt_furthest(&self, stmt: &Stmt) -> usize {
        let key = std::ptr::from_ref(stmt) as usize;
        if let Some(&known) = self.furthest.borrow().get(&key) {
            return known;
        }
        let furthest = self.furthest(Item::Stmt(stmt), false);
        self.remember(&self.furthest, key, furthest);
        furthest
    }

    /// The start of the member name `name` that follows a receiver whose
    /// last token starts at `last`, after `.`, `&.` or `::` and any
    /// closing brackets and line breaks, or of the `)` that ends it when
    /// `parenthesized`; `last` when the tokens do not show one.
    fn name_after(&self, last: usize, name: &str, parenthesized: bool) -> usize {
        let Some(mut index) = self.token_at(last) else {
            return last;
        };
        index += 1;
        let limit = self.tokens.len().min(index + 64);
        while index < limit {
            if self.step(1) {
                return last;
            }
            match &self.tokens[index].kind {
                TokenKind::Punct(')' | ']' | '}') | TokenKind::Newline => index += 1,
                TokenKind::Punct('.') | TokenKind::Operator("&." | "::") => break,
                _ => return last,
            }
        }
        index += 1;
        while index < limit && self.tokens[index].kind == TokenKind::Newline {
            index += 1;
        }
        if index >= limit || self.text(index) != name {
            return last;
        }
        let at = self.tokens[index].span.start;
        if parenthesized
            && self.tokens.get(index + 1).map(|token| &token.kind) == Some(&TokenKind::Punct('('))
            && self.tokens.get(index + 2).map(|token| &token.kind) == Some(&TokenKind::Punct(')'))
        {
            return self.tokens[index + 2].span.start;
        }
        at
    }

    /// The operator token of a binary expression located at `offset`, all
    /// of it, although two-character operators are located at their last.
    pub fn operator(&self, offset: usize) -> Span {
        self.containing(offset)
    }

    /// The span of the whole token containing `offset`, such as a binary
    /// operator's, which is located at its last character.
    pub fn containing(&self, offset: usize) -> Span {
        let index = self.token_from(offset + 1).checked_sub(1);
        match index.map(|index| &self.tokens[index].span) {
            Some(span) if span.start <= offset && offset < span.end => {
                Span::new(span.start, span.end)
            }
            _ => self.token(offset),
        }
    }
}

/// The start of an expression's first token. Binary operators, ranges,
/// indexes and ternaries record their operator's position, so their span
/// starts at their leftmost operand.
pub(crate) fn first_offset(expr: &Expr) -> usize {
    let mut first = expr.offset as usize;
    let mut current = expr;
    loop {
        current = match &current.node {
            Node::Binary(_, left, _) | Node::Range(Some(left), _, _) => left,
            Node::Index(receiver, _)
            | Node::Member(receiver, _)
            | Node::SafeMember(receiver, _)
            | Node::Method(receiver, _, _, _)
            | Node::SafeMethod(receiver, _, _, _)
            | Node::Scope(receiver, _, _)
            | Node::BlockCall(receiver, _)
            | Node::ComputedCall(receiver, _) => receiver,
            Node::Conditional(branches, _) if !branches.is_empty() => &branches[0].0,
            _ => break,
        };
        first = first.min(current.offset as usize);
    }
    first
}
