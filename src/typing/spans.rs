//! Source spans of syntax nodes, which record only where they start: the
//! parser's tokens give each node's end.

use crate::{
    diagnostic::Span,
    syntax::{Argument, Block, CallForm, Expr, Node, Statement, Stmt, Target},
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
    tokens: &'a [Token],
    /// Tokens and nodes visited, for [`super::Checked::steps`].
    pub steps: std::cell::Cell<u64>,
    /// [`last_offset`] by node, so shared subtrees are walked once.
    lasts: std::cell::RefCell<std::collections::HashMap<usize, usize>>,
}

impl<'a> Spans<'a> {
    pub fn new(source: &'a str, tokens: &'a [Token]) -> Self {
        Self {
            source,
            tokens,
            steps: std::cell::Cell::new(0),
            lasts: std::cell::RefCell::default(),
        }
    }

    fn step(&self, count: usize) {
        self.steps.set(self.steps.get() + count as u64);
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
            self.step(1);
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
        let last = match value {
            Some(value) => self.last(value).max(stmt_last(stmt)),
            None => stmt_last(stmt),
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
                self.step(1);
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
        self.step(last_index.saturating_sub(first) + 1);
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
            self.step(1);
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
            self.step(1);
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
                _ => {
                    let (last, visited) = last_offset_counted(current, &self.lasts);
                    self.step(visited);
                    break last;
                }
            };
            path.push((current, trail));
            current = next;
        };
        let key = std::ptr::from_ref(current) as usize;
        self.lasts.borrow_mut().insert(key, position);
        for (node, trail) in path.into_iter().rev() {
            if let Trail::Member(name, parenthesized) = trail {
                position = self.name_after(position, name, parenthesized);
            }
            let key = std::ptr::from_ref(node) as usize;
            self.lasts.borrow_mut().insert(key, position);
        }
        position
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
            self.step(1);
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

    /// The offset of the operator token of a binary expression starting at
    /// `offset`.
    pub fn operator(&self, offset: usize) -> Span {
        self.token(offset)
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

/// The start of the last token-bearing child of an expression.
pub(crate) fn last_offset(expr: &Expr) -> usize {
    last_offset_counted(expr, &std::cell::RefCell::default()).0
}

/// [`last_offset`], reusing the results `memo` holds for subtrees, and the
/// number of nodes it visited.
fn last_offset_counted(
    root: &Expr,
    memo: &std::cell::RefCell<std::collections::HashMap<usize, usize>>,
) -> (usize, usize) {
    let mut last = root.offset as usize;
    let mut visited = 0;
    let mut pending = vec![root];
    let mut visit_stmts: Vec<&Stmt> = Vec::new();
    while let Some(expr) = pending.pop() {
        visited += 1;
        last = last.max(expr.offset as usize);
        if !std::ptr::eq(expr, root) {
            if let Some(&known) = memo.borrow().get(&(std::ptr::from_ref(expr) as usize)) {
                last = last.max(known);
                continue;
            }
        }
        match &expr.node {
            Node::Try(attempt) => {
                visit_stmts.extend(attempt.body.iter());
                visit_stmts.extend(attempt.alternate.iter());
                visit_stmts.extend(attempt.ensure.iter());
                for rescue in attempt.rescues.iter() {
                    last = last.max(rescue.offset as usize);
                    visit_stmts.extend(rescue.body.iter());
                }
            }
            Node::Shape(_, Some(fallback), _) => pending.push(fallback),
            Node::Template(values, _) | Node::Array(values) | Node::Yield(values) => {
                pending.extend(values.iter())
            }
            Node::Hash(entries) => pending.extend(entries.iter().map(|(_, value)| value)),
            Node::Unary(_, value) => pending.push(value),
            Node::Binary(_, left, right) => {
                pending.push(left);
                pending.push(right);
            }
            Node::Range(start, end, _) => {
                pending.extend(start.as_deref());
                pending.extend(end.as_deref());
            }
            Node::Conditional(branches, alternate) => {
                for (condition, value) in branches.iter() {
                    pending.push(condition);
                    pending.push(value);
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
            Node::Compound(stmt) => visit_stmts.push(stmt),
            Node::Call(_, args, _) => pending.extend(arguments(args)),
            Node::ComputedCall(receiver, args) => {
                pending.push(receiver);
                pending.extend(arguments(args));
            }
            Node::BlockCall(call, block) => {
                pending.push(call);
                last = last.max(block_last(block));
            }
            Node::Member(receiver, _) | Node::SafeMember(receiver, _) => pending.push(receiver),
            Node::Scope(receiver, _, args) => {
                pending.push(receiver);
                if let Some(args) = args {
                    pending.extend(arguments(args));
                }
            }
            Node::Method(receiver, _, args, _) | Node::SafeMethod(receiver, _, args, _) => {
                pending.push(receiver);
                pending.extend(arguments(args));
            }
            Node::Index(receiver, selectors) => {
                pending.push(receiver);
                pending.extend(selectors.iter());
            }
            _ => (),
        }
        while let Some(stmt) = visit_stmts.pop() {
            visited += 1;
            last = last.max(stmt_last(stmt));
        }
    }
    (last, visited)
}

fn arguments(args: &[Argument]) -> impl Iterator<Item = &Expr> {
    args.iter().map(|arg| &arg.value)
}

fn block_last(block: &Block) -> usize {
    let mut last = block.offset as usize;
    for stmt in block.body.iter() {
        last = last.max(stmt_last(stmt));
    }
    for target in block.params.iter() {
        last = last.max(target_last(target));
    }
    last
}

fn target_last(target: &Target) -> usize {
    match target {
        Target::Value(expr) => last_offset(expr),
        Target::Typed(target, _) => target_last(target),
        Target::Tuple(parts) => parts
            .iter()
            .filter_map(|(part, _)| part.as_ref().map(target_last))
            .max()
            .unwrap_or(0),
    }
}

/// The start of the last token-bearing part of a statement.
pub(crate) fn stmt_last(stmt: &Stmt) -> usize {
    let own = stmt.offset as usize;
    match &stmt.node {
        Statement::Expr(expr) => own.max(last_offset(expr)),
        Statement::Assign(target, _, value) => own.max(target_last(target)).max(last_offset(value)),
        Statement::If(branches, alternate, _) => {
            let mut last = own;
            for (condition, body) in branches.iter() {
                last = last.max(last_offset(condition));
                for stmt in body.iter() {
                    last = last.max(stmt_last(stmt));
                }
            }
            for stmt in alternate.iter() {
                last = last.max(stmt_last(stmt));
            }
            last
        }
        Statement::While(condition, body, _) => body
            .iter()
            .map(stmt_last)
            .fold(own.max(last_offset(condition)), usize::max),
        Statement::For(target, iterable, body) => body.iter().map(stmt_last).fold(
            own.max(target_last(target)).max(last_offset(iterable)),
            usize::max,
        ),
        Statement::Return(value) | Statement::Break(value) | Statement::Next(value) => {
            own.max(value.as_ref().map_or(0, last_offset))
        }
        Statement::Raise(value, message) => own
            .max(value.as_deref().map_or(0, last_offset))
            .max(message.as_deref().map_or(0, last_offset)),
        _ => own,
    }
}
