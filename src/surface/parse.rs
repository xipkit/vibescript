//! Parses the compiler's token stream into a [`Tree`] with source spans.
//!
//! The grammar follows the compiler's parser decision for decision: which
//! line a suffix may continue on, when an identifier takes command
//! arguments, and which call a `do` or brace block attaches to. The tokens
//! come from [`crate::tooling::tokens`], after the compiler's own regex
//! and percent-literal re-reads, so only structure is decided here. Error
//! reporting is not mirrored: a source this parser refuses is left alone.

use super::syntax::*;
use crate::tooling::{self, TokenKind};
use std::collections::HashSet;

/// Why a source could not be parsed, which a test's failure shows.
#[derive(Debug)]
#[allow(dead_code)]
pub struct Fail {
    pub offset: usize,
    pub message: String,
}

type Result<T> = std::result::Result<T, Fail>;

/// Parses `source`, which must already compile.
#[cfg(test)]
pub fn parse(source: &str) -> Result<Tree> {
    let tokens = lex(source, 0).map_err(|error| Fail {
        offset: 0,
        message: error.to_string(),
    })?;
    let mut parser = Parser::new(source, tokens);
    parser.declare_types();
    let body = parser.program()?;
    Ok(Tree {
        tokens: parser.tokens,
        body,
    })
}

/// Parses `source` from the tokens the compiler read, refusing nesting
/// deeper than `limit` statements and expressions. The parser recurses once
/// per level, so the limit bounds the stack it needs.
pub fn parse_tokens(source: &str, tokens: &[tooling::Token], limit: usize) -> Result<Tree> {
    let mut parser = Parser::new(source, convert(source, tokens, 0));
    parser.declare_types();
    parser.limit = limit;
    let body = parser.program();
    // A speculative parse that failed at the limit may have been retried
    // another way, so any refusal refuses the source.
    if let Some(offset) = parser.too_deep {
        return Err(Fail {
            offset,
            message: "nesting too deep".to_owned(),
        });
    }
    let body = body?;
    Ok(Tree {
        tokens: parser.tokens,
        body,
    })
}

fn lex(source: &str, base: usize) -> crate::Result<Vec<Token>> {
    Ok(convert(source, &tooling::tokens(source)?, base))
}

fn convert(source: &str, tokens: &[tooling::Token], base: usize) -> Vec<Token> {
    tokens
        .iter()
        .map(|token| Token {
            kind: token.kind.clone(),
            start: token.span.start + base,
            end: token.span.end + base,
            line: token.line,
            end_line: token.line + source[token.span.clone()].matches('\n').count(),
        })
        .collect()
}

const KEYWORDS: [&str; 34] = [
    "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure", "enum",
    "export", "false", "for", "getter", "if", "in", "next", "nil", "private", "property", "raise",
    "rescue", "retry", "return", "self", "setter", "then", "true", "unless", "until", "when",
    "while", "yield",
];

pub fn keyword(w: &str) -> bool {
    KEYWORDS.binary_search(&w).is_ok()
}

/// `expr` without the parentheses around it, which the compiler's parser
/// keeps no node for, so that a receiver or callee decides as it does there.
fn ungrouped(expr: &Expr) -> &Expr {
    let mut expr = expr;
    while let ExprKind::Group(_, inner, _) = &expr.kind {
        expr = inner;
    }
    expr
}

/// [`ungrouped`], taking the expression.
fn ungroup(expr: Expr) -> Expr {
    let mut expr = expr;
    while let ExprKind::Group(_, inner, _) = expr.kind {
        expr = *inner;
    }
    expr
}

fn reserved(w: &str) -> bool {
    matches!(
        w,
        "class"
            | "enum"
            | "for"
            | "in"
            | "when"
            | "until"
            | "begin"
            | "rescue"
            | "ensure"
            | "def"
            | "export"
            | "unless"
            | "case"
            | "yield"
            | "retry"
            | "raise"
            | "end"
            | "else"
            | "elsif"
            | "do"
            | "then"
            | "if"
            | "while"
            | "return"
            | "break"
            | "next"
    )
}

fn assignment(op: &str) -> bool {
    matches!(
        op,
        "=" | "+=" | "-=" | "*=" | "/=" | "//=" | "%=" | "**=" | "||=" | "&&="
    )
}

pub fn binding_power(op: &str) -> Option<(u8, u8)> {
    Some(match op {
        "||" => (3, 4),
        "&&" => (4, 5),
        "==" | "!=" | "===" | "=~" | "!~" => (5, 6),
        "<" | "<=" | ">" | ">=" | "<=>" => (6, 7),
        ".." | "..." => (7, 8),
        "&" => (9, 10),
        "<<" => (10, 11),
        "+" | "-" => (11, 12),
        "*" | "/" | "//" | "%" => (12, 13),
        "**" => (14, 14),
        _ => return None,
    })
}

/// A block's `|` pipes, when written.
type Pipes = Option<(Tok, Tok)>;

/// The next step of an expression after its first operand.
enum Suffix {
    Rescue,
    Command,
    Block(bool),
    Call,
    Scope,
    Member,
    Index,
    Ternary,
    Binary(u8),
}

#[derive(Clone, Copy, PartialEq)]
enum Place {
    Statement,
    For,
    Group(bool),
}

struct Parser<'s> {
    source: &'s str,
    tokens: Vec<Token>,
    pos: usize,
    groups: usize,
    line_exprs: usize,
    command_depth: usize,
    ternaries: Vec<usize>,
    command_group: usize,
    loop_condition: Option<usize>,
    then_stop: Option<usize>,
    locals: HashSet<String>,
    declared_it: bool,
    inside_class: bool,
    nesting: usize,
    call_end: usize,
    /// The typed block parameter the latest parameter list ended with.
    block_param: Option<Span>,
    /// The bare `*` the latest parameter list's keyword parameters follow.
    keyword_star: Option<Tok>,
    /// Where this parser's tokens start: an interpolation's come after the
    /// source's own.
    floor: usize,
    /// How many statements and expressions enclose the one being parsed.
    depth: usize,
    /// The deepest nesting the parser accepts.
    limit: usize,
    /// Where the nesting limit was first exceeded.
    too_deep: Option<usize>,
    /// The type aliases, classes and enums the source declares anywhere.
    type_names: std::rc::Rc<HashSet<String>>,
}

/// Parser state that a speculative parse restores.
struct Saved {
    pos: usize,
    groups: usize,
    line_exprs: usize,
    command_depth: usize,
    ternaries: Vec<usize>,
    command_group: usize,
    loop_condition: Option<usize>,
    then_stop: Option<usize>,
    locals: HashSet<String>,
    declared_it: bool,
    call_end: usize,
}

impl<'s> Parser<'s> {
    fn new(source: &'s str, tokens: Vec<Token>) -> Self {
        Self {
            source,
            tokens,
            pos: 0,
            groups: 0,
            line_exprs: 0,
            command_depth: 0,
            ternaries: Vec::new(),
            command_group: 0,
            loop_condition: None,
            then_stop: None,
            locals: HashSet::new(),
            declared_it: false,
            inside_class: false,
            nesting: 0,
            call_end: 0,
            block_param: None,
            keyword_star: None,
            floor: 0,
            depth: 0,
            limit: usize::MAX,
            too_deep: None,
            type_names: std::rc::Rc::default(),
        }
    }

    /// Records the type aliases, classes and enums the source declares, as
    /// the compiler's parser does, which read as types where a default
    /// value could also be meant.
    fn declare_types(&mut self) {
        let mut names = HashSet::new();
        for index in 0..self.tokens.len().saturating_sub(2) {
            let Some(word) = self.word_at(index) else {
                continue;
            };
            if !matches!(word, "type" | "class" | "enum") || !self.ident(index + 1) {
                continue;
            }
            if word == "type" && !self.is_op(index + 2, "=") {
                continue;
            }
            names.insert(self.text(index + 1).to_owned());
        }
        self.type_names = std::rc::Rc::new(names);
    }

    fn save(&self) -> Saved {
        Saved {
            pos: self.pos,
            groups: self.groups,
            line_exprs: self.line_exprs,
            command_depth: self.command_depth,
            ternaries: self.ternaries.clone(),
            command_group: self.command_group,
            loop_condition: self.loop_condition,
            then_stop: self.then_stop,
            locals: self.locals.clone(),
            declared_it: self.declared_it,
            call_end: self.call_end,
        }
    }

    fn restore(&mut self, saved: Saved) {
        self.pos = saved.pos;
        self.groups = saved.groups;
        self.line_exprs = saved.line_exprs;
        self.command_depth = saved.command_depth;
        self.ternaries = saved.ternaries;
        self.command_group = saved.command_group;
        self.loop_condition = saved.loop_condition;
        self.then_stop = saved.then_stop;
        self.locals = saved.locals;
        self.declared_it = saved.declared_it;
        self.call_end = saved.call_end;
    }

    // Token queries -------------------------------------------------------

    fn kind(&self) -> &TokenKind {
        &self.tokens[self.pos].kind
    }

    fn kind_at(&self, index: usize) -> &TokenKind {
        &self.tokens[index.min(self.tokens.len() - 1)].kind
    }

    fn text(&self, index: usize) -> &'s str {
        let token = &self.tokens[index];
        &self.source[token.start..token.end]
    }

    fn word_at(&self, index: usize) -> Option<&'s str> {
        (*self.kind_at(index) == TokenKind::Word).then(|| self.text(index))
    }

    fn is_word(&self, index: usize, word: &str) -> bool {
        self.word_at(index) == Some(word)
    }

    fn at_word(&self, word: &str) -> bool {
        self.is_word(self.pos, word)
    }

    fn is_p(&self, index: usize, c: char) -> bool {
        *self.kind_at(index) == TokenKind::Punct(c)
    }

    fn at_p(&self, c: char) -> bool {
        self.is_p(self.pos, c)
    }

    fn is_op(&self, index: usize, op: &str) -> bool {
        matches!(self.kind_at(index), TokenKind::Operator(o) if *o == op)
    }

    fn at_op(&self, op: &str) -> bool {
        self.is_op(self.pos, op)
    }

    fn end_line(&self, index: usize) -> bool {
        matches!(
            self.kind_at(index),
            TokenKind::Newline | TokenKind::Semicolon
        )
    }

    fn newline(&self, index: usize) -> bool {
        *self.kind_at(index) == TokenKind::Newline
    }

    fn eof(&self, index: usize) -> bool {
        *self.kind_at(index) == TokenKind::Eof
    }

    fn fail<T>(&self, message: &str) -> Result<T> {
        Err(Fail {
            offset: self.tokens[self.pos.min(self.tokens.len() - 1)].start,
            message: message.to_owned(),
        })
    }

    fn bump(&mut self) -> Tok {
        let at = self.pos;
        if !self.eof(at) {
            self.pos += 1;
        }
        at
    }

    fn take_word(&mut self, word: &str) -> Option<Tok> {
        self.at_word(word).then(|| self.bump())
    }

    fn expect_word(&mut self, word: &str) -> Result<Tok> {
        match self.take_word(word) {
            Some(tok) => Ok(tok),
            None => self.fail(&format!("expected {word}")),
        }
    }

    fn take_p(&mut self, c: char) -> Option<Tok> {
        self.at_p(c).then(|| self.bump())
    }

    fn expect_p(&mut self, c: char) -> Result<Tok> {
        match self.take_p(c) {
            Some(tok) => Ok(tok),
            None => self.fail(&format!("expected {c}")),
        }
    }

    fn significant(&self, mut index: usize) -> usize {
        while index + 1 < self.tokens.len() && self.newline(index) {
            index += 1;
        }
        index
    }

    fn lines(&mut self) {
        while self.end_line(self.pos) {
            self.pos += 1;
        }
    }

    fn line_breaks(&mut self) {
        while self.newline(self.pos) {
            self.pos += 1;
        }
    }

    fn previous_index(&self) -> usize {
        let mut index = self.pos;
        while index > self.floor {
            index -= 1;
            if !self.end_line(index) {
                return index;
            }
        }
        self.floor
    }

    fn previous(&self) -> &Token {
        &self.tokens[self.previous_index()]
    }

    /// The end of the last token before the current one, for spans.
    fn last_end(&self) -> usize {
        let mut index = self.pos;
        while index > self.floor {
            index -= 1;
            if !self.end_line(index) {
                return self.tokens[index].end;
            }
        }
        self.tokens[self.floor].start
    }

    fn start(&self) -> usize {
        self.tokens[self.pos].start
    }

    fn ident(&self, index: usize) -> bool {
        self.word_at(index)
            .is_some_and(|w| !keyword(w) && !w.starts_with('@'))
    }

    fn prefix(&self, index: usize) -> bool {
        match self.kind_at(index) {
            TokenKind::Word => {
                let w = self.text(index);
                if w.starts_with('@') {
                    true
                } else if keyword(w) {
                    matches!(
                        w,
                        "then"
                            | "true"
                            | "false"
                            | "nil"
                            | "self"
                            | "yield"
                            | "if"
                            | "unless"
                            | "case"
                            | "begin"
                            | "for"
                            | "while"
                            | "until"
                    )
                } else {
                    true
                }
            }
            TokenKind::Integer
            | TokenKind::Float
            | TokenKind::String(_)
            | TokenKind::Template(_)
            | TokenKind::Words { .. }
            | TokenKind::Regex
            | TokenKind::Symbol { .. }
            | TokenKind::Punct('(' | '[' | '{') => true,
            TokenKind::Operator(op) => matches!(*op, "!" | "-" | "+" | "->"),
            _ => false,
        }
    }

    fn literal_token(&self, index: usize) -> bool {
        matches!(
            self.kind_at(index),
            TokenKind::Integer
                | TokenKind::Float
                | TokenKind::String(_)
                | TokenKind::Regex
                | TokenKind::Template(_)
                | TokenKind::Words { .. }
                | TokenKind::Symbol { .. }
        )
    }

    fn starts_expression(&self) -> bool {
        match self.kind() {
            TokenKind::Word => {
                let w = self.text(self.pos);
                if w == "then" {
                    return self.then_stop != Some(self.groups);
                }
                !reserved(w)
                    || matches!(
                        w,
                        "if" | "unless" | "case" | "while" | "until" | "for" | "yield" | "begin"
                    )
            }
            TokenKind::Punct('(' | '[' | '{') => true,
            TokenKind::Operator(op) => matches!(*op, "+" | "-" | "!"),
            _ => self.literal_token(self.pos),
        }
    }

    fn keyword_label(&self, pos: usize) -> bool {
        self.word_at(pos).is_some_and(|w| !w.starts_with('@'))
            && pos + 1 < self.tokens.len()
            && self.is_p(pos + 1, ':')
    }

    fn command_argument_start(&self, pos: usize, after_comma: bool) -> bool {
        if self.keyword_label(pos) {
            return true;
        }
        match self.kind_at(pos) {
            TokenKind::Word => {
                let w = self.text(pos);
                if w == "then" {
                    return self.then_stop != Some(self.groups);
                }
                !reserved(w) || matches!(w, "case" | "for" | "yield" | "begin")
            }
            TokenKind::Operator("!") => true,
            TokenKind::Punct('[') | TokenKind::Operator("*" | "**" | "&") => after_comma,
            _ => self.literal_token(pos),
        }
    }

    fn can_attach_do(&self) -> bool {
        (self.command_depth == 0 || self.groups > self.command_group)
            && self.loop_condition.is_none_or(|group| self.groups > group)
    }

    fn ends_after(&self, index: usize, line: usize) -> bool {
        let next = self.significant(index + 1);
        match self.kind_at(next) {
            TokenKind::Eof | TokenKind::Punct('}') | TokenKind::Newline | TokenKind::Semicolon => {
                true
            }
            TokenKind::Word
                if matches!(
                    self.text(next),
                    "end" | "else" | "elsif" | "ensure" | "rescue"
                ) =>
            {
                true
            }
            _ => self.tokens[next].line != line,
        }
    }

    fn modifier_follows(&self, line: usize) -> bool {
        let next = self.significant(self.pos);
        self.tokens[next].line == line
            && self
                .word_at(next)
                .is_some_and(|w| matches!(w, "if" | "unless" | "while" | "until"))
    }

    fn comma_follows(&mut self) -> bool {
        let comma = self.significant(self.pos);
        if !self.is_p(comma, ',') {
            return false;
        }
        self.pos = comma + 1;
        true
    }

    fn comma_on_line(&self) -> bool {
        self.at_p(',') && self.tokens[self.pos].line == self.previous().line
    }

    fn ternary_separator(&mut self) {
        let mut next = self.pos;
        while self.newline(next) {
            next += 1;
        }
        if self.is_p(next, ':') {
            self.pos = next;
        }
    }

    fn negative_literal(&self, sign: usize) -> bool {
        if sign + 1 >= self.tokens.len() {
            return false;
        }
        self.tokens[sign].end == self.tokens[sign + 1].start
            && matches!(
                self.kind_at(sign + 1),
                TokenKind::Integer | TokenKind::Float
            )
            && !(sign + 2..self.tokens.len())
                .find(|&i| !self.newline(i))
                .is_some_and(|i| self.is_op(i, "**"))
    }

    fn assertion(&self) -> bool {
        self.at_word("assert")
            && !(self.is_p(self.pos + 1, '(')
                && self.tokens[self.pos + 1].start == self.tokens[self.pos].end)
    }

    fn assignment_ahead(&self) -> bool {
        let mut nesting = 0usize;
        let mut comma = false;
        let mut after_member_separator = false;
        for i in self.pos..self.tokens.len() {
            match self.kind_at(i) {
                TokenKind::Punct('(' | '[' | '{') => nesting += 1,
                TokenKind::Punct(')' | ']' | '}') => {
                    if nesting == 0 {
                        return false;
                    }
                    nesting -= 1;
                }
                TokenKind::Operator(op) if nesting == 0 && assignment(op) => return true,
                TokenKind::Semicolon if nesting == 0 => return false,
                TokenKind::Newline if nesting == 0 => {
                    if after_member_separator {
                        continue;
                    }
                    let next = (i + 1..self.tokens.len()).find(|&j| !self.end_line(j));
                    let continues = next.is_some_and(|j| {
                        self.is_p(j, '.')
                            || self.is_op(j, "&.")
                            || matches!(self.kind_at(j), TokenKind::Operator(op) if assignment(op))
                    });
                    if !comma && !continues {
                        return false;
                    }
                }
                TokenKind::Word
                    if nesting == 0
                        && reserved(self.text(i))
                        && self.text(i) != "then"
                        && !after_member_separator =>
                {
                    return false;
                }
                TokenKind::Eof => return false,
                _ => (),
            }
            if !self.end_line(i) {
                comma = self.is_p(i, ',');
                after_member_separator = self.is_p(i, '.') || self.is_op(i, "&.");
            }
        }
        false
    }

    fn statement_continues(&self) -> bool {
        let next = &self.tokens[self.pos];
        let continues = match &next.kind {
            TokenKind::Punct('.' | '(' | '[' | '?') => true,
            TokenKind::Operator(op) => matches!(*op, "&." | "::") || binding_power(op).is_some(),
            TokenKind::Word => matches!(self.text(self.pos), "do" | "rescue"),
            _ => false,
        };
        continues && next.line == self.previous().end_line
    }

    fn limit_continues(&self, index: usize) -> bool {
        match self.kind_at(index) {
            TokenKind::Punct('.' | '?') => true,
            TokenKind::Operator("*") => !self.splat_assignment_ahead(index),
            TokenKind::Operator("+" | "-") => {
                let sign = &self.tokens[index];
                let next = &self.tokens[self.significant(index + 1)];
                next.kind != TokenKind::Eof
                    && (next.line > sign.line || (next.line == sign.line && next.start > sign.end))
            }
            TokenKind::Operator(op) => matches!(
                *op,
                "&." | "::"
                    | "/"
                    | "//"
                    | "**"
                    | "%"
                    | ".."
                    | "..."
                    | "=="
                    | "==="
                    | "!="
                    | "=~"
                    | "!~"
                    | "<"
                    | "<="
                    | ">"
                    | ">="
                    | "<=>"
                    | "&&"
                    | "||"
                    | "<<"
                    | "&"
            ),
            _ => false,
        }
    }

    fn continuation_position(&self, min: u8) -> Option<usize> {
        if !self.end_line(self.pos) {
            return None;
        }
        let mut next = self.pos;
        while self.end_line(next) {
            if !self.newline(next) {
                return None;
            }
            next += 1;
        }
        let continues = match self.kind_at(next) {
            TokenKind::Word if self.text(next) == "do" => {
                (self.line_exprs == 0 && self.can_attach_do()) || self.pos == self.call_end
            }
            TokenKind::Word if self.text(next) == "rescue" => {
                self.line_exprs == 0 && self.groups > 0 && min == 0 && !self.keyword_label(next)
            }
            TokenKind::Punct('.') => true,
            TokenKind::Operator("::" | "&.") => true,
            TokenKind::Punct('?') => min <= 2,
            TokenKind::Punct('(' | '[') => self.line_exprs == 0 && self.groups > 0,
            TokenKind::Operator(op) => {
                let (left, _) = binding_power(op)?;
                if left < min {
                    return None;
                }
                if self.line_exprs == 0 {
                    if matches!(*op, "/" | "//") {
                        return None;
                    }
                    return (self.groups > 0).then_some(next);
                }
                match *op {
                    "+" | "-" => {
                        let lexeme = &self.tokens[next];
                        (next + 1..self.tokens.len())
                            .find(|&i| !self.newline(i))
                            .is_some_and(|i| {
                                let operand = &self.tokens[i];
                                !matches!(
                                    operand.kind,
                                    TokenKind::Eof | TokenKind::Semicolon | TokenKind::Newline
                                ) && (operand.line > lexeme.end_line || operand.start > lexeme.end)
                            })
                    }
                    "*" => !self.splat_assignment_ahead(next),
                    "/" | "//" => false,
                    _ => true,
                }
            }
            _ => false,
        };
        continues.then_some(next)
    }

    fn splat_assignment_ahead(&self, start: usize) -> bool {
        let mut groups = 0usize;
        let mut previous = start;
        let operand = start + 1;
        let shaped = self.is_p(operand, ',')
            || self.is_op(operand, "=")
            || self.tokens[operand].start == self.tokens[previous].end;
        let mut comma = false;
        for i in start + 1..self.tokens.len() {
            let token = &self.tokens[i];
            if token.line > self.tokens[start].line + 64 {
                return false;
            }
            if self.end_line(i) {
                if !self.newline(i) {
                    return false;
                }
                continue;
            }
            let previous_token = &self.tokens[previous];
            if token.line > previous_token.end_line
                && groups == 0
                && !self.is_p(previous, ',')
                && !((shaped || comma)
                    && (self.is_op(i, "=")
                        || ((self.is_p(i, '.') || self.is_op(i, "&."))
                            && (self.word_at(previous).is_some()
                                || self.is_p(previous, ')')
                                || self.is_p(previous, ']')))))
            {
                return false;
            }
            if groups == 0 && !self.is_op(i, "=") {
                let allowed = if self.is_p(previous, '.') || self.is_op(previous, "&.") {
                    self.word_at(i).is_some()
                } else {
                    match &token.kind {
                        TokenKind::Word => !reserved(self.text(i)),
                        TokenKind::Punct(',' | '.' | '(' | ')' | '[' | ']') => true,
                        TokenKind::Operator("*" | "&.") => true,
                        _ => false,
                    }
                };
                if !allowed {
                    return false;
                }
            }
            match &token.kind {
                TokenKind::Punct('(' | '[') => groups += 1,
                TokenKind::Punct(')' | ']') => {
                    let Some(next) = groups.checked_sub(1) else {
                        return false;
                    };
                    groups = next;
                }
                TokenKind::Operator("=") if groups == 0 => return true,
                TokenKind::Punct(',') if groups == 0 => comma = true,
                TokenKind::Eof => return false,
                _ => (),
            }
            previous = i;
        }
        false
    }

    /// Whether the `{` at the current token starts `lhs`'s block: it follows
    /// a `)`, or `lhs` is a call and not a value such as a local, a
    /// constant or a literal.
    fn block_follows(&self, lhs: &Expr) -> bool {
        self.is_p(self.pos - 1, ')') || self.block_owner(lhs)
    }

    fn block_owner(&self, lhs: &Expr) -> bool {
        let lowercase = |name: &str| {
            !name
                .chars()
                .next()
                .is_some_and(crate::syntax::unicode::upper)
        };
        match &ungrouped(lhs).kind {
            ExprKind::Name(name) => lowercase(name) && !self.locals.contains(name),
            ExprKind::Call(call) => {
                !call.scoped(&self.tokens) || call.args.is_some() || lowercase(&call.name)
            }
            ExprKind::Computed(..) => true,
            _ => false,
        }
    }

    fn command_start(&self, lhs: &Expr, min: u8) -> bool {
        if self.line_exprs == 0 || min > 14 {
            return false;
        }
        let lhs = ungrouped(lhs);
        let local = match &lhs.kind {
            ExprKind::Name(name) => self.locals.contains(name),
            // A scoped function takes arguments too, as in `Math::sqrt 9`.
            ExprKind::Call(call)
                if call.receiver.is_some()
                    && call.args.is_none()
                    && call.block.is_none()
                    && (!call.scoped(&self.tokens)
                        || !call
                            .name
                            .chars()
                            .next()
                            .is_some_and(crate::syntax::unicode::upper)) =>
            {
                false
            }
            _ => return false,
        };
        let previous = self.previous();
        let next = &self.tokens[self.pos];
        if next.line != previous.end_line {
            return false;
        }
        if self.keyword_label(self.pos) {
            return true;
        }
        let (previous_end, next_start) = (previous.end, next.start);
        match &next.kind {
            TokenKind::Punct('[') => !local && previous_end != next_start,
            TokenKind::Words { .. } => {
                let implicit =
                    matches!(&lhs.kind, ExprKind::Name(name) if name == "it") && !self.declared_it;
                (!local || implicit) && previous_end != next_start
            }
            TokenKind::Regex => !local && previous_end != next_start,
            TokenKind::Operator(op @ ("/" | "//")) => {
                !local
                    && previous_end != next_start
                    && (self
                        .source
                        .as_bytes()
                        .get(next.end)
                        .is_some_and(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
                        || (*op == "//" && (self.end_line(self.pos + 1) || self.eof(self.pos + 1))))
            }
            TokenKind::Operator(op @ ("*" | "**" | "&")) => {
                let start = next_start + usize::from(*op == "**");
                !local
                    && previous_end != start
                    && self.pos + 1 < self.tokens.len()
                    && !self.end_line(self.pos + 1)
                    && !self.eof(self.pos + 1)
                    && self.tokens[self.pos + 1].line == next.end_line
                    && self.tokens[self.pos + 1].start == next.end
            }
            _ => self.command_argument_start(self.pos, false),
        }
    }

    // Programs and statements ---------------------------------------------

    fn program(&mut self) -> Result<Vec<Stmt>> {
        let mut body = Vec::new();
        loop {
            self.lines();
            if self.eof(self.pos) {
                break;
            }
            body.push(self.declaration()?);
        }
        Ok(body)
    }

    fn alias_ahead(&self) -> bool {
        let next = &self.tokens[self.pos + 1];
        self.at_word("alias")
            && next.line == self.tokens[self.pos].line
            && (self.ident(self.pos + 1) || matches!(next.kind, TokenKind::Symbol { .. }))
    }

    fn module_ahead(&self) -> bool {
        self.at_word("module")
            && self.ident(self.pos + 1)
            && self.tokens[self.pos].line == self.tokens[self.pos + 1].line
    }

    fn declaration(&mut self) -> Result<Stmt> {
        self.nested(Self::unnested_declaration)
    }

    /// Runs `parse` one level deeper, failing past the nesting limit.
    fn nested<T>(&mut self, parse: impl FnOnce(&mut Self) -> Result<T>) -> Result<T> {
        if self.depth >= self.limit {
            self.too_deep.get_or_insert(self.start());
            return self.fail("nesting too deep");
        }
        self.depth += 1;
        let result = parse(self);
        self.depth -= 1;
        result
    }

    fn unnested_declaration(&mut self) -> Result<Stmt> {
        let start = self.start();
        let word = match self.word_at(self.pos) {
            Some(w) if matches!(w, "def" | "class" | "enum" | "export" | "private") => Some(w),
            Some("alias") if self.alias_ahead() => Some("alias"),
            Some("type")
                if self.ident(self.pos + 1)
                    && self.is_op(self.pos + 2, "=")
                    && self.tokens[self.pos + 1].line == self.tokens[self.pos].line =>
            {
                Some("type")
            }
            Some(_) if self.module_ahead() => Some("module"),
            _ => None,
        };
        let Some(word) = word else {
            return self.plain();
        };
        let kind = match word {
            "def" => StmtKind::Def(Box::new(self.function(false, None)?)),
            "class" => StmtKind::Class(Box::new(self.class_like(false)?)),
            "module" => StmtKind::Class(Box::new(self.class_like(true)?)),
            "enum" => StmtKind::Enum(self.enumeration()?),
            "alias" => {
                self.alias_names()?;
                StmtKind::Other
            }
            "type" => {
                self.pos += 3;
                self.line_breaks();
                self.type_expr(1, false)?;
                StmtKind::Other
            }
            _ => {
                let modifier = self.bump();
                self.line_breaks();
                if !self.at_word("def") {
                    return self.fail("expected def");
                }
                StmtKind::Def(Box::new(self.function(false, Some(modifier))?))
            }
        };
        self.reject_modifier()?;
        Ok(Stmt {
            span: Span {
                start,
                end: self.last_end(),
            },
            kind,
        })
    }

    fn reject_modifier(&mut self) -> Result<()> {
        if self
            .word_at(self.pos)
            .is_some_and(|w| matches!(w, "if" | "unless" | "while" | "until"))
        {
            return self.fail("modifier after a declaration");
        }
        Ok(())
    }

    fn alias_names(&mut self) -> Result<()> {
        let line = self.tokens[self.pos].line;
        self.bump();
        for _ in 0..2 {
            self.pos = self.significant(self.pos);
            if self.tokens[self.pos].line == line
                && (self.ident(self.pos) || matches!(self.kind(), TokenKind::Symbol { .. }))
            {
                self.bump();
            } else {
                return self.fail("expected alias name");
            }
        }
        Ok(())
    }

    fn plain(&mut self) -> Result<Stmt> {
        let start = self.start();
        let first = self.pos;
        let starts_begin = self.at_word("begin");
        let mut stmt = if starts_begin {
            self.begin_statement()?
        } else {
            self.plain_statement()?
        };
        if matches!(
            stmt.kind,
            StmtKind::If(..) | StmtKind::While(..) | StmtKind::For(..)
        ) {
            stmt = self.continued_statement(stmt)?;
        }
        let Some(modifier) = self
            .word_at(self.pos)
            .filter(|w| matches!(*w, "if" | "unless" | "while" | "until"))
        else {
            return Ok(stmt);
        };
        let bare_begin = starts_begin
            && matches!(
                &stmt.kind,
                StmtKind::Expr(Expr {
                    kind: ExprKind::Begin(_),
                    ..
                })
            );
        let allowed = matches!(
            stmt.kind,
            StmtKind::Expr(_)
                | StmtKind::Raise(..)
                | StmtKind::Retry(_)
                | StmtKind::Assign(..)
                | StmtKind::Flow(..)
        );
        if bare_begin || !allowed {
            return self.fail("modifier is only supported after expressions");
        }
        let keyword = self.bump();
        let condition = self.line_expr(0)?;
        let kind = match modifier {
            "if" => ModifierKind::If,
            "unless" => ModifierKind::Unless,
            "while" => ModifierKind::While,
            _ => ModifierKind::Until,
        };
        let _ = first;
        Ok(Stmt {
            span: Span {
                start,
                end: condition.span.end,
            },
            kind: StmtKind::Modifier(Modifier {
                body: Box::new(stmt),
                keyword,
                kind,
                condition,
            }),
        })
    }

    fn continued_statement(&mut self, stmt: Stmt) -> Result<Stmt> {
        if !self.statement_continues() {
            return Ok(stmt);
        }
        let span = stmt.span;
        let expr = Expr {
            span,
            kind: ExprKind::Loop(Box::new(stmt)),
        };
        let expr = self.statement_tail(expr)?;
        Ok(Stmt {
            span: expr.span,
            kind: StmtKind::Expr(expr),
        })
    }

    fn begin_statement(&mut self) -> Result<Stmt> {
        let keyword = self.bump();
        let attempt = self.begin_expression(keyword)?;
        let expr = if self.statement_continues() {
            self.statement_tail(attempt)?
        } else {
            attempt
        };
        Ok(Stmt {
            span: expr.span,
            kind: StmtKind::Expr(expr),
        })
    }

    fn statement_tail(&mut self, expr: Expr) -> Result<Expr> {
        let open = self.line_exprs == 0;
        if open {
            self.groups += 1;
        }
        let result = self.expr_tail(expr, 0, None, None, true);
        if open {
            self.groups -= 1;
        }
        result
    }

    fn plain_statement(&mut self) -> Result<Stmt> {
        let start = self.start();
        let keyword = [
            "raise", "retry", "if", "unless", "while", "until", "for", "return", "break", "next",
        ]
        .into_iter()
        .find(|k| self.at_word(k));
        let span = |p: &Self| Span {
            start,
            end: p.last_end(),
        };
        match keyword {
            Some("raise") => {
                let keyword = self.bump();
                let kind = self.raise_statement(keyword)?;
                Ok(Stmt {
                    span: span(self),
                    kind,
                })
            }
            Some("retry") => {
                let keyword = self.bump();
                Ok(Stmt {
                    span: span(self),
                    kind: StmtKind::Retry(keyword),
                })
            }
            Some(w @ ("if" | "unless")) => {
                let keyword = self.bump();
                let node = self.if_stmt(keyword, w == "unless")?;
                Ok(Stmt {
                    span: span(self),
                    kind: StmtKind::If(node),
                })
            }
            Some(w @ ("while" | "until")) => {
                let keyword = self.bump();
                let node = self.while_stmt(keyword, w == "until")?;
                Ok(Stmt {
                    span: span(self),
                    kind: StmtKind::While(node),
                })
            }
            Some("for") => {
                self.bump();
                let node = self.for_stmt()?;
                Ok(Stmt {
                    span: span(self),
                    kind: StmtKind::For(node),
                })
            }
            Some(_) => {
                let keyword = self.bump();
                let value = self.flow_value(keyword)?;
                Ok(Stmt {
                    span: span(self),
                    kind: StmtKind::Flow(keyword, value),
                })
            }
            None if self.typed_local_ahead() => self.typed_local(),
            None if self.assertion() => {
                let expr = self.assertion_call()?;
                Ok(Stmt {
                    span: expr.span,
                    kind: StmtKind::Expr(expr),
                })
            }
            None if self.at_op("*") || self.assignment_ahead() => self.assignment_statement(),
            None => {
                let first = self.pos;
                let expr = self.line_expr(0)?;
                let next = self.significant(self.pos);
                let target = match self.kind_at(next) {
                    TokenKind::Punct(',') => true,
                    TokenKind::Operator(op) => assignment(op),
                    _ => false,
                };
                if target {
                    self.pos = first;
                    return self.assignment_statement();
                }
                let expr = self.trailing_block(expr)?;
                Ok(Stmt {
                    span: expr.span,
                    kind: StmtKind::Expr(expr),
                })
            }
        }
    }

    /// Whether the name at `index` is followed by a colon written as an
    /// annotation's: attached to the name and followed by a space.
    fn annotation_colon(&self, index: usize) -> bool {
        index + 1 < self.tokens.len()
            && self.is_p(index + 1, ':')
            && self.tokens[index + 1].start == self.tokens[index].end
            && matches!(
                self.source.as_bytes().get(self.tokens[index + 1].end),
                Some(b' ' | b'\t')
            )
    }

    /// Whether a statement declares a typed local or constant,
    /// `name: T = value`.
    fn typed_local_ahead(&mut self) -> bool {
        if !self.ident(self.pos) || !self.annotation_colon(self.pos) {
            return false;
        }
        let saved = self.save();
        self.pos += 2;
        let typed = self.type_expr(1, false).is_ok() && self.is_op(self.significant(self.pos), "=");
        self.restore(saved);
        typed
    }

    fn typed_local(&mut self) -> Result<Stmt> {
        let start = self.start();
        let name = self.bump();
        let token = &self.tokens[name];
        let target = Target::Expr(Expr {
            span: Span {
                start: token.start,
                end: token.end,
            },
            kind: ExprKind::Name(self.text(name).to_owned()),
        });
        self.bump();
        let ty = self.type_expr(1, false)?;
        self.pos = self.significant(self.pos);
        let op = self.bump();
        self.line_breaks();
        let value = self.block_line_expr()?;
        let target = Target::Typed(Box::new(target), ty);
        self.declare_target(&target);
        Ok(Stmt {
            span: Span {
                start,
                end: self.last_end(),
            },
            kind: StmtKind::Assign(Assign {
                targets: vec![target],
                op,
                values: vec![value],
            }),
        })
    }

    fn assertion_call(&mut self) -> Result<Expr> {
        let start = self.start();
        let line = self.tokens[self.pos].line;
        let name_tok = self.bump();
        if self.ends_after(self.pos - 1, line) {
            return Ok(Expr {
                span: Span {
                    start,
                    end: self.tokens[name_tok].end,
                },
                kind: ExprKind::Name("assert".to_owned()),
            });
        }
        self.line_breaks();
        let mut items = Vec::new();
        loop {
            let value = self.line_expr(0)?;
            items.push(Arg {
                kind: ArgKind::Positional,
                span: value.span,
                value,
            });
            if !self.comma_follows() {
                break;
            }
            self.line_breaks();
        }
        Ok(Expr {
            span: Span {
                start,
                end: self.last_end(),
            },
            kind: ExprKind::Call(Box::new(Call {
                receiver: None,
                operator: None,
                name: "assert".to_owned(),
                name_tok,
                args: Some(Args {
                    parens: None,
                    items,
                }),
                block: None,
            })),
        })
    }

    fn raise_statement(&mut self, keyword: Tok) -> Result<StmtKind> {
        let line = self.tokens[keyword].line;
        if self.ends_after(self.pos - 1, line) || self.modifier_follows(line) {
            return Ok(StmtKind::Raise(keyword, None, None));
        }
        self.line_breaks();
        let value = self.line_expr(0)?;
        let separated = self.tokens[self.pos].line == self.previous().end_line && self.at_p(',');
        let message = if separated {
            self.bump();
            self.line_breaks();
            Some(self.line_expr(0)?)
        } else {
            None
        };
        Ok(StmtKind::Raise(keyword, Some(value), message))
    }

    fn flow_value(&mut self, keyword: Tok) -> Result<Option<Expr>> {
        let line = self.tokens[keyword].line;
        let value = !self.ends_after(self.pos - 1, line) && !self.modifier_follows(line);
        if !value {
            return Ok(None);
        }
        self.line_breaks();
        let first = self.line_expr(0)?;
        if self.text(keyword) != "return" || !self.comma_on_line() {
            return Ok(Some(first));
        }
        let start = first.span.start;
        let mut items = vec![first];
        while self.comma_on_line() {
            self.bump();
            self.line_breaks();
            items.push(self.line_expr(0)?);
        }
        Ok(Some(Expr {
            span: Span {
                start,
                end: self.last_end(),
            },
            kind: ExprKind::Array(items),
        }))
    }

    fn assignment_statement(&mut self) -> Result<Stmt> {
        let start = self.start();
        let first = self.pos;
        let (target, tuple) = self.target(Place::Statement)?;
        let next = self.significant(self.pos);
        if matches!(self.kind_at(next), TokenKind::Operator(op) if assignment(op)) {
            self.pos = next;
        }
        let op = match self.kind() {
            TokenKind::Operator(op) if assignment(op) => Some(self.pos),
            _ => None,
        };
        let Some(op) = op else {
            if tuple {
                return self.fail("parallel assignment targets require '='");
            }
            self.pos = first;
            let expr = self.block_line_expr()?;
            return Ok(Stmt {
                span: expr.span,
                kind: StmtKind::Expr(expr),
            });
        };
        self.pos += 1;
        self.line_breaks();
        let first_value = self.block_line_expr()?;
        let mut values = vec![first_value];
        if tuple && self.comma_follows() {
            loop {
                let comma = self.pos - 1;
                if self.ends_after(comma, self.tokens[comma].line) {
                    break;
                }
                self.line_breaks();
                values.push(self.block_line_expr()?);
                if !self.comma_follows() {
                    break;
                }
            }
        }
        self.declare_target(&target);
        let targets = match target {
            Target::Group(_, parts) if tuple => parts,
            target => vec![target],
        };
        Ok(Stmt {
            span: Span {
                start,
                end: self.last_end(),
            },
            kind: StmtKind::Assign(Assign {
                targets,
                op,
                values,
            }),
        })
    }

    fn declare_target(&mut self, target: &Target) {
        let mut names = Vec::new();
        target.names(&mut |name, _| names.push(name.to_owned()));
        for name in names {
            self.declared_it |= name == "it";
            self.locals.insert(name);
        }
    }

    /// Parses a destructuring target list; a tuple comes back as a group
    /// with an empty span, and whether commas made it one.
    fn target(&mut self, place: Place) -> Result<(Target, bool)> {
        let typed = place == Place::Group(true);
        let mut parts = Vec::new();
        let mut tuple = false;
        let start = self.start();
        loop {
            let star = self.pos;
            let rest = self.at_op("*");
            let mut anonymous = false;
            if rest {
                let next = self.significant(self.pos + 1);
                anonymous = match self.kind_at(next) {
                    TokenKind::Punct(',' | ')' | ']') => true,
                    TokenKind::Operator(op) => assignment(op),
                    TokenKind::Word => place == Place::For && self.text(next) == "in",
                    _ => false,
                };
                self.bump();
                if !anonymous {
                    self.line_breaks();
                }
                tuple = true;
            }
            let grouped = place != Place::Statement || !parts.is_empty() || rest;
            let group = match self.kind() {
                TokenKind::Punct('(') if grouped && !anonymous => Some(')'),
                TokenKind::Punct('[') if grouped && !anonymous => Some(']'),
                _ => None,
            };
            let mut value = if anonymous {
                None
            } else if let Some(close) = group {
                let open = self.start();
                self.bump();
                self.line_breaks();
                let (inner, inner_tuple) = self.target(Place::Group(typed))?;
                self.line_breaks();
                self.expect_p(close)?;
                let span = Span {
                    start: open,
                    end: self.last_end(),
                };
                let parts = match inner {
                    Target::Group(_, parts) if inner_tuple => parts,
                    inner => vec![inner],
                };
                Some(Target::Group(span, parts))
            } else {
                Some(Target::Expr(self.line_expr(0)?))
            };
            let colon = self.significant(self.pos);
            if typed && value.is_some() && self.is_p(colon, ':') {
                self.pos = colon + 1;
                self.line_breaks();
                let ty = self.type_expr(1, false)?;
                value = Some(Target::Typed(Box::new(value.take().unwrap()), ty));
            }
            let part = if rest {
                Target::Splat(star, value.map(Box::new))
            } else {
                value.unwrap()
            };
            parts.push(part);
            let comma = self.significant(self.pos);
            if !self.is_p(comma, ',') {
                break;
            }
            self.pos = comma + 1;
            tuple = true;
            self.line_breaks();
        }
        let target = if tuple {
            Target::Group(
                Span {
                    start,
                    end: self.last_end(),
                },
                parts,
            )
        } else {
            parts.pop().unwrap()
        };
        Ok((target, tuple))
    }

    fn condition(&mut self) -> Result<Expr> {
        self.line_breaks();
        let previous = self.then_stop.replace(self.groups);
        let condition = self.line_expr(0);
        self.then_stop = previous;
        condition
    }

    fn if_stmt(&mut self, keyword: Tok, unless: bool) -> Result<If> {
        let mut branches = Vec::new();
        let alternate = loop {
            let condition = self.condition()?;
            self.take_word("then");
            self.lines();
            let body = self.block(&["else", "elsif", "end"])?;
            branches.push((condition, body));
            if self.take_word("elsif").is_some() {
                continue;
            }
            if let Some(tok) = self.take_word("else") {
                self.lines();
                let body = self.block(&["end"])?;
                break Some((tok, body));
            }
            break None;
        };
        let end = self.expect_word("end")?;
        Ok(If {
            keyword,
            unless,
            branches,
            alternate,
            end,
        })
    }

    fn expression_body(&mut self) -> Result<Vec<Stmt>> {
        let expr = self.expr(0)?;
        let expr = self.trailing_block(expr)?;
        Ok(vec![Stmt {
            span: expr.span,
            kind: StmtKind::Expr(expr),
        }])
    }

    fn if_expr(&mut self, keyword: Tok, unless: bool) -> Result<If> {
        let mut branches = Vec::new();
        let alternate = loop {
            let condition = self.condition()?;
            self.take_word("then");
            self.lines();
            let body = self.expression_body()?;
            branches.push((condition, body));
            self.lines();
            if self.take_word("elsif").is_some() {
                continue;
            }
            if let Some(tok) = self.take_word("else") {
                self.lines();
                let body = self.expression_body()?;
                self.lines();
                break Some((tok, body));
            }
            break None;
        };
        let end = self.expect_word("end")?;
        Ok(If {
            keyword,
            unless,
            branches,
            alternate,
            end,
        })
    }

    fn while_stmt(&mut self, keyword: Tok, until: bool) -> Result<While> {
        self.line_breaks();
        let previous = self.loop_condition.replace(self.groups);
        let condition = self.line_expr(0);
        self.loop_condition = previous;
        let condition = condition?;
        self.take_word("do");
        let body = self.block(&["end"])?;
        let end = self.expect_word("end")?;
        Ok(While {
            keyword,
            until,
            condition,
            body,
            end,
        })
    }

    fn for_stmt(&mut self) -> Result<For> {
        let (target, _) = self.target(Place::For)?;
        self.line_breaks();
        self.expect_word("in")?;
        self.line_breaks();
        let previous = self.loop_condition.replace(self.groups);
        let iterable = self.line_expr(0);
        self.loop_condition = previous;
        let iterable = iterable?;
        self.take_word("do");
        self.declare_target(&target);
        let body = self.block(&["end"])?;
        self.expect_word("end")?;
        Ok(For {
            target,
            iterable,
            body,
        })
    }

    fn block(&mut self, stop: &[&str]) -> Result<Vec<Stmt>> {
        let mut body = Vec::new();
        self.nesting += 1;
        loop {
            self.lines();
            if self.word_at(self.pos).is_some_and(|w| stop.contains(&w))
                || (self.at_p('}') && stop.contains(&"}"))
            {
                break;
            }
            if self.eof(self.pos) {
                return self.fail("expected end");
            }
            body.push(self.statement()?);
        }
        self.nesting -= 1;
        Ok(body)
    }

    fn statement(&mut self) -> Result<Stmt> {
        self.declaration()
    }

    // Declarations --------------------------------------------------------

    fn function(&mut self, constants: bool, modifier: Option<Tok>) -> Result<Def> {
        let def_tok = self.bump();
        let def_line = self.tokens[def_tok].line;
        self.line_breaks();
        let (mut name, name_span, class_method, operator) = self.function_name()?;
        self.line_breaks();
        let mut name_end = name_span.end;
        if self.at_op("=") && (!operator || name == "[]") {
            let tok = self.bump();
            name_end = self.tokens[tok].end;
            self.line_breaks();
            name.push('=');
        }
        let outer_locals = std::mem::take(&mut self.locals);
        if constants {
            for local in &outer_locals {
                if local.chars().next().is_some_and(char::is_uppercase) {
                    self.locals.insert(local.clone());
                }
            }
        }
        let outer_it = std::mem::replace(&mut self.declared_it, false);
        let line = self.tokens[self.pos].line;
        let parenthesized = self.at_p('(') && line == def_line;
        let bare = !parenthesized
            && line == def_line
            && match self.kind() {
                TokenKind::Word => {
                    let w = self.text(self.pos);
                    !keyword(w) && !w.starts_with("@@")
                }
                TokenKind::Operator("*" | "**" | "&") => true,
                _ => false,
            };

        let mut signature = def_line;
        let mut parens = None;
        self.block_param = None;
        self.keyword_star = None;
        let params = if parenthesized {
            let open = self.bump();
            self.line_breaks();
            let params = if self.at_p(')') {
                Vec::new()
            } else {
                self.groups += 1;
                let params = self.parameters(true)?;
                self.groups -= 1;
                self.line_breaks();
                if !self.at_p(')') {
                    return self.fail("expected )");
                }
                params
            };
            signature = self.tokens[self.pos].line;
            let close = self.bump();
            parens = Some((open, close));
            params
        } else if bare {
            let params = self.parameters(false)?;
            signature = self.previous().line;
            params
        } else {
            Vec::new()
        };
        let block = self.block_param.take();
        let star = self.keyword_star.take();
        let arrow = self.significant(self.pos);
        let result = if self.is_op(arrow, "->") && self.tokens[arrow].line == signature {
            self.pos = arrow + 1;
            self.line_breaks();
            Some((arrow, self.type_expr(1, false)?))
        } else {
            None
        };
        let body = self.block(&["rescue", "else", "ensure", "end"])?;
        let rescue = if self.word_at(self.pos).is_some_and(|w| w != "end") {
            Some(self.rescue_tail()?)
        } else {
            None
        };
        let end = self.expect_word("end")?;
        self.locals = outer_locals;
        self.declared_it = outer_it;
        Ok(Def {
            keyword: def_tok,
            modifier,
            name,
            name_span: Span {
                start: name_span.start,
                end: name_end,
            },
            class_method,
            params,
            star,
            block,
            parens,
            result,
            body,
            rescue,
            end,
        })
    }

    fn function_name(&mut self) -> Result<(String, Span, bool, bool)> {
        let start = self.start();
        if self.at_word("self") && self.is_p(self.significant(self.pos + 1), '.') {
            self.bump();
            self.line_breaks();
            self.bump();
            self.line_breaks();
            if !self.ident(self.pos) {
                return self.fail("expected identifier");
            }
            let tok = self.bump();
            return Ok((
                self.text(tok).to_owned(),
                Span {
                    start,
                    end: self.tokens[tok].end,
                },
                true,
                false,
            ));
        }
        let operator = match self.kind() {
            TokenKind::Operator(
                op @ ("+" | "-" | "*" | "/" | "%" | "**" | "<<" | "&" | "==" | "!=" | "<" | "<="
                | ">" | ">=" | "<=>"),
            ) => Some(*op),
            TokenKind::Punct('[') if self.is_p(self.significant(self.pos + 1), ']') => Some("[]"),
            _ => None,
        };
        if let Some(op) = operator {
            if op == "[]" {
                self.bump();
                self.line_breaks();
            }
            let tok = self.bump();
            return Ok((
                op.to_owned(),
                Span {
                    start,
                    end: self.tokens[tok].end,
                },
                false,
                true,
            ));
        }
        if !self.ident(self.pos) {
            return self.fail("expected function name");
        }
        let tok = self.bump();
        Ok((
            self.text(tok).to_owned(),
            Span {
                start,
                end: self.tokens[tok].end,
            },
            false,
            false,
        ))
    }

    fn parameters(&mut self, parenthesized: bool) -> Result<Vec<Param>> {
        let mut params: Vec<Param> = Vec::new();
        loop {
            // A bare `*` makes the parameters after it keyword parameters.
            if self.at_op("*") && self.is_p(self.significant(self.pos + 1), ',') {
                if self.keyword_star.is_some()
                    || params
                        .iter()
                        .any(|param| param.kind != ParamKind::Positional)
                {
                    return self.fail("misplaced bare *");
                }
                self.keyword_star = Some(self.bump());
                self.pos = self.significant(self.pos) + 1;
                self.line_breaks();
                continue;
            }
            if self.at_op("&")
                && self.word_at(self.pos + 1).is_some()
                && self.annotation_colon(self.pos + 1)
            {
                let start = self.start();
                self.pos += 3;
                self.line_breaks();
                if self.take_p('(').is_some() {
                    self.line_breaks();
                    while self.take_p(')').is_none() {
                        self.type_expr(1, false)?;
                        self.line_breaks();
                        self.take_p(',');
                        self.line_breaks();
                    }
                } else {
                    self.type_expr(1, false)?;
                }
                let arrow = self.significant(self.pos);
                if self.is_op(arrow, "->") {
                    self.pos = arrow + 1;
                    self.line_breaks();
                    self.type_expr(1, false)?;
                }
                self.block_param = Some(Span {
                    start,
                    end: self.last_end(),
                });
                break;
            }
            let strict = self.keyword_star.is_some();
            let mut param = self.parameter(parenthesized, strict)?;
            let rest = params.iter().any(|param| param.kind == ParamKind::Rest);
            if (strict || rest) && param.kind == ParamKind::Positional {
                param.kind = ParamKind::Keyword;
            }
            self.locals.insert(param.name.clone());
            self.declared_it |= param.name == "it";
            params.push(param);
            let comma = self.significant(self.pos);
            if !self.is_p(comma, ',') {
                break;
            }
            self.pos = comma + 1;
            self.line_breaks();
        }
        Ok(params)
    }

    /// Parses one parameter; a `strict` one follows a bare `*` and is
    /// written only as `name`, `name: T`, `name = default` or
    /// `name: T = default`.
    fn parameter(&mut self, parenthesized: bool, strict: bool) -> Result<Param> {
        let start = self.start();
        let mut kind = match self.kind() {
            TokenKind::Operator("*") => {
                self.bump();
                let next = self.significant(self.pos);
                if self.is_op(next, "*") {
                    self.pos = next + 1;
                    ParamKind::KeywordRest
                } else {
                    ParamKind::Rest
                }
            }
            TokenKind::Operator("**") => {
                self.bump();
                ParamKind::KeywordRest
            }
            TokenKind::Operator("&") => return self.fail("block capture parameter"),
            _ => ParamKind::Positional,
        };
        self.line_breaks();
        let instance = self
            .word_at(self.pos)
            .is_some_and(|w| w.starts_with('@') && !w.starts_with("@@"));
        if !self.ident(self.pos) && !(instance && kind == ParamKind::Positional) {
            return self.fail("expected parameter name");
        }
        let name_tok = self.bump();
        let word = self.text(name_tok);
        let name = word.strip_prefix('@').unwrap_or(word).to_owned();
        let mut ty = None;
        let colon = self.significant(self.pos);
        let mut keyword_colon = None;
        if self.is_p(colon, ':') {
            let plain = kind == ParamKind::Positional && !instance && !strict;
            self.pos = colon + 1;
            if plain && self.ends_required_keyword(colon, parenthesized) {
                return Ok(Param {
                    kind: ParamKind::Keyword,
                    name,
                    name_tok,
                    instance,
                    ty: None,
                    default: None,
                    keyword_colon: Some(colon),
                    span: Span {
                        start,
                        end: self.last_end(),
                    },
                });
            }
            if plain && self.keyword_default(parenthesized)? {
                self.line_breaks();
                let default = if parenthesized {
                    self.expr(0)?
                } else {
                    self.line_expr(0)?
                };
                return Ok(Param {
                    kind: ParamKind::Keyword,
                    name,
                    name_tok,
                    instance,
                    ty: None,
                    default: Some(default),
                    keyword_colon: Some(colon),
                    span: Span {
                        start,
                        end: self.last_end(),
                    },
                });
            }
            self.line_breaks();
            ty = Some(self.type_expr(1, false)?);
            let trailing = self.significant(self.pos);
            if plain && self.is_p(trailing, ':') {
                keyword_colon = Some(trailing);
                self.pos = trailing + 1;
                if !self.ends_required_keyword(trailing, parenthesized) {
                    // A typed keyword with a default, `name: T: = value`.
                    let equals = self.significant(self.pos);
                    if !self.is_op(equals, "=") {
                        return self.fail("typed required keyword must end after ':'");
                    }
                }
                kind = ParamKind::Keyword;
            }
        }
        let equals = self.significant(self.pos);
        let default = if self.is_op(equals, "=") {
            self.pos = equals + 1;
            self.line_breaks();
            Some(if parenthesized {
                self.expr(0)?
            } else {
                self.line_expr(0)?
            })
        } else {
            None
        };
        Ok(Param {
            kind,
            name,
            name_tok,
            instance,
            ty,
            default,
            keyword_colon,
            span: Span {
                start,
                end: self.last_end(),
            },
        })
    }

    fn ends_required_keyword(&self, colon: usize, parenthesized: bool) -> bool {
        let next = self.significant(colon + 1);
        match self.kind_at(next) {
            TokenKind::Punct(',' | ')') => true,
            TokenKind::Operator("->")
            | TokenKind::Newline
            | TokenKind::Semicolon
            | TokenKind::Eof => !parenthesized,
            _ => !parenthesized && self.tokens[next].line != self.tokens[colon].line,
        }
    }

    fn keyword_default(&mut self, parenthesized: bool) -> Result<bool> {
        let peek = self.significant(self.pos);
        Ok(match self.kind_at(peek) {
            // `name: nil` declares a parameter of type nil, as the compiler reads it.
            TokenKind::Word if self.text(peek) == "nil" => false,
            // A bracket reads as a tuple type only when every leaf names a type.
            TokenKind::Punct('[') if !self.tuple_start(peek + 1) => true,
            TokenKind::Punct('[') => {
                let saved = self.save();
                self.pos = peek;
                let annotation = match self.type_expr(1, false) {
                    Ok(ty) => {
                        self.declared_leaves(&ty)
                            && !self.default_field(&ty)
                            && self.type_boundary(self.pos - 1, parenthesized)
                    }
                    Err(_) => false,
                };
                self.restore(saved);
                !annotation
            }
            TokenKind::Punct('{') => {
                let saved = self.save();
                self.pos = peek;
                let annotation = match self.type_expr(1, false) {
                    Ok(ty) => {
                        !self.default_field(&ty) && self.type_boundary(self.pos - 1, parenthesized)
                    }
                    Err(_) => false,
                };
                self.restore(saved);
                !annotation
            }
            _ if self.ident(peek) => self.name_starts_default(peek, parenthesized),
            _ => self.prefix(peek),
        })
    }

    /// Whether the token at `index` can start a tuple type's first element:
    /// a builtin type name or a type the source declares.
    fn tuple_start(&self, index: usize) -> bool {
        self.word_at(index).is_some_and(|name| self.type_name(name))
    }

    /// Whether `name` names a builtin type, one of the signature table's
    /// aliases or a type the source declares.
    fn type_name(&self, name: &str) -> bool {
        crate::types::builtin_name(name).is_some()
            || crate::signatures::alias_type(name).is_some()
            || self.type_names.contains(name)
    }

    /// Whether every name in a type is one [`Self::type_name`] accepts, so
    /// the type cannot also read as a value.
    fn declared_leaves(&self, ty: &TypeExpr) -> bool {
        match &ty.kind {
            TypeKind::Named(tok, arguments) if arguments.is_empty() => {
                self.type_name(self.text(*tok).trim_end_matches('?'))
            }
            TypeKind::Named(_, arguments) => arguments.iter().all(|a| self.declared_leaves(a)),
            TypeKind::Qualified(_) => false,
            TypeKind::Shape(fields, _) => fields.iter().all(|(_, f)| self.declared_leaves(f)),
            TypeKind::Union(options) | TypeKind::Tuple(options) => {
                options.iter().all(|option| self.declared_leaves(option))
            }
        }
    }

    fn name_starts_default(&self, peek: usize, parenthesized: bool) -> bool {
        let name = self.text(peek);
        let mut next = self.significant(peek + 1);
        // The compiler's lexer splits `int?=nil` so that `?=` never ends a
        // name; the adjacent `?` is still the type's optional marker.
        if self.is_p(next, '?') && self.tokens[next].start == self.tokens[peek].end {
            next = self.significant(next + 1);
        }
        match self.kind_at(next) {
            TokenKind::Punct(',' | ')' | ':' | '|') | TokenKind::Operator("=") => false,
            TokenKind::Operator("<") => {
                !crate::types::builtin_name(name)
                    .is_some_and(crate::types::BuiltinName::takes_type_arguments)
                    && self.locals.contains(name)
            }
            TokenKind::Punct('.') => !self.dotted_type_follows(peek, next, parenthesized),
            TokenKind::Operator("::") => !self.scoped_type_follows(peek, parenthesized),
            TokenKind::Operator("->")
            | TokenKind::Newline
            | TokenKind::Semicolon
            | TokenKind::Eof => parenthesized,
            _ => parenthesized || self.tokens[next].line == self.tokens[peek].line,
        }
    }

    /// Whether `Outer::Inner` after a parameter's colon reads as a nested
    /// class's type rather than a default.
    fn scoped_type_follows(&self, peek: usize, parenthesized: bool) -> bool {
        let mut last = peek;
        loop {
            let scope = self.significant(last + 1);
            if !self.is_op(scope, "::") {
                break;
            }
            let member = self.significant(scope + 1);
            if !self.ident(member) {
                return false;
            }
            last = member;
        }
        let question = self.significant(last + 1);
        if self.is_p(question, '?') {
            last = question;
        }
        self.type_boundary(last, parenthesized)
    }

    fn dotted_type_follows(&self, peek: usize, dot: usize, parenthesized: bool) -> bool {
        let namespace = self.text(peek);
        if self.locals.contains(namespace) {
            return false;
        }
        let member = self.significant(dot + 1);
        if !self.ident(member) {
            return false;
        }
        let written = self.text(member);
        let name = written.trim_end_matches('?');
        let looks_like_type = name.as_bytes().first().is_some_and(u8::is_ascii_uppercase)
            && name.as_bytes().iter().skip(1).any(u8::is_ascii_lowercase);
        if !looks_like_type || (namespace == "Math" && matches!(name, "PI" | "E")) {
            return false;
        }
        if written.ends_with('?') {
            return self.type_boundary(member, parenthesized);
        }
        let question = self.significant(member + 1);
        let last = if self.is_p(question, '?') {
            question
        } else {
            member
        };
        self.type_boundary(last, parenthesized)
    }

    fn type_boundary(&self, last: usize, parenthesized: bool) -> bool {
        let next = self.significant(last + 1);
        match self.kind_at(next) {
            TokenKind::Punct(',' | ')' | ':' | '|') | TokenKind::Operator("=") => true,
            TokenKind::Operator("->")
            | TokenKind::Newline
            | TokenKind::Semicolon
            | TokenKind::Eof => !parenthesized,
            _ => !parenthesized && self.tokens[next].line != self.tokens[last].line,
        }
    }

    /// Whether a type could also read as a value: an empty or nil shape
    /// field, or a name that is a local.
    fn default_field(&self, ty: &TypeExpr) -> bool {
        match &ty.kind {
            TypeKind::Shape(fields, false) => {
                fields.is_empty() || fields.iter().any(|(_, field)| self.default_field(field))
            }
            TypeKind::Named(tok, args) => {
                let name = self.text(*tok).trim_end_matches('?');
                if name == "nil" {
                    return true;
                }
                args.is_empty() && !ty.nullable && self.locals.contains(name)
            }
            _ => false,
        }
    }

    fn enumeration(&mut self) -> Result<Enum> {
        self.bump();
        self.line_breaks();
        if !self.ident(self.pos) {
            return self.fail("expected identifier");
        }
        let name = {
            let tok = self.bump();
            self.text(tok).to_owned()
        };
        let mut members = Vec::new();
        loop {
            self.lines();
            if self.eof(self.pos) || self.at_word("end") {
                break;
            }
            if !self.ident(self.pos) && !self.at_word("enum") {
                return self.fail("expected enum member name");
            }
            members.push({
                let tok = self.bump();
                self.text(tok).to_owned()
            });
        }
        self.expect_word("end")?;
        Ok(Enum { name, members })
    }

    fn class_like(&mut self, module: bool) -> Result<Class> {
        let keyword = self.bump();
        self.pos = self.significant(self.pos);
        if !self.ident(self.pos) {
            return self.fail("expected identifier");
        }
        let name_tok = self.bump();
        let name = self.text(name_tok).to_owned();
        let outer_locals = std::mem::take(&mut self.locals);
        let outer_it = std::mem::replace(&mut self.declared_it, false);
        let outer_class = std::mem::replace(&mut self.inside_class, true);
        self.nesting += 1;
        let mut members = Vec::new();
        loop {
            self.lines();
            if self.eof(self.pos) || self.at_word("end") {
                break;
            }
            let start = self.start();
            let word = self.word_at(self.pos).unwrap_or("");
            if word.starts_with('@') && !word.starts_with("@@") && self.annotation_colon(self.pos) {
                let name = word.trim_start_matches('@').to_owned();
                self.pos += 2;
                let ty = self.type_expr(1, false)?;
                let equals = self.significant(self.pos);
                let default = if self.is_op(equals, "=")
                    && self.tokens[equals].line == self.tokens[start_token(self, start)].line
                {
                    self.pos = equals + 1;
                    self.line_breaks();
                    Some(self.line_expr(0)?)
                } else {
                    None
                };
                members.push(Member::Ivar(name, ty, default));
                continue;
            }
            if word.starts_with("@@") && self.annotation_colon(self.pos) {
                let name = word.trim_start_matches('@').to_owned();
                self.pos += 2;
                let ty = self.type_expr(1, false)?;
                let equals = self.significant(self.pos);
                if !self.is_op(equals, "=") {
                    return self.fail("class variable declaration needs a value");
                }
                self.pos = equals + 1;
                self.line_breaks();
                let value = self.line_expr(0)?;
                members.push(Member::ClassVar(name, ty, value));
                continue;
            }
            match word {
                "def" => members.push(Member::Def(self.function(true, None)?)),
                "alias" if self.alias_ahead() => {
                    self.alias_names()?;
                    members.push(Member::Other(Span {
                        start,
                        end: self.last_end(),
                    }));
                }
                "alias_method" => {
                    self.alias_method()?;
                    members.push(Member::Other(Span {
                        start,
                        end: self.last_end(),
                    }));
                }
                "public" | "protected" | "private"
                    if word == "private" || self.visibility_directive() =>
                {
                    if self.visibility_member()? {
                        let modifier = Some(self.pos);
                        let _ = modifier;
                    }
                    members.push(Member::Other(Span {
                        start,
                        end: self.last_end(),
                    }));
                }
                "module" if module && self.module_ahead() => {
                    members.push(Member::Class(self.class_like(true)?));
                }
                "property" | "getter" | "setter" => {
                    members.push(Member::Property(self.class_properties()?));
                }
                _ => {
                    let stmt = self.declaration()?;
                    members.push(match stmt.kind {
                        StmtKind::Class(class) => Member::Class(*class),
                        StmtKind::Def(def) => Member::Def(*def),
                        _ => Member::Stmt(stmt),
                    });
                }
            }
        }
        self.inside_class = outer_class;
        self.nesting -= 1;
        let end = self.expect_word("end")?;
        self.locals = outer_locals;
        self.declared_it = outer_it;
        Ok(Class {
            keyword,
            module,
            name,
            name_tok,
            members,
            end,
        })
    }

    fn alias_method(&mut self) -> Result<()> {
        self.bump();
        let parenthesized = self.is_p(self.significant(self.pos), '(');
        if parenthesized {
            self.line_breaks();
            self.bump();
        }
        for index in 0..2 {
            self.line_breaks();
            if !matches!(self.kind(), TokenKind::Symbol { .. }) {
                return self.fail("expected symbol");
            }
            self.bump();
            if index == 0 {
                self.line_breaks();
                self.expect_p(',')?;
            }
        }
        if parenthesized {
            self.line_breaks();
            self.expect_p(')')?;
        }
        Ok(())
    }

    fn inline_visibility(&self) -> bool {
        let next = self.significant(self.pos + 1);
        self.tokens[next].line == self.tokens[self.pos].line
            && self
                .word_at(next)
                .is_some_and(|w| matches!(w, "def" | "property" | "getter" | "setter"))
    }

    fn visibility_directive(&self) -> bool {
        let Some(word) = self.word_at(self.pos) else {
            return false;
        };
        let line = self.tokens[self.pos].line;
        let next = self.significant(self.pos + 1);
        if self.inline_visibility()
            || (matches!(self.kind_at(next), TokenKind::Symbol { .. })
                && self.tokens[next].line == line)
        {
            return true;
        }
        !self.locals.contains(word) && self.ends_after(self.pos, line)
    }

    /// Parses a visibility word; true when it applies to the declaration after it.
    fn visibility_member(&mut self) -> Result<bool> {
        let line = self.tokens[self.pos].line;
        if self.inline_visibility() {
            self.bump();
            self.line_breaks();
            return Ok(true);
        }
        let next = self.significant(self.pos + 1);
        if matches!(self.kind_at(next), TokenKind::Symbol { .. }) && self.tokens[next].line == line
        {
            self.pos = next;
            loop {
                self.bump();
                let comma = self.significant(self.pos);
                if !self.is_p(comma, ',') {
                    return Ok(false);
                }
                self.pos = self.significant(comma + 1);
            }
        }
        self.bump();
        Ok(false)
    }

    fn class_properties(&mut self) -> Result<Property> {
        let start = self.start();
        let keyword = self.bump();
        self.line_breaks();
        let mut names = Vec::new();
        loop {
            if !self.ident(self.pos) {
                return self.fail("expected property name");
            }
            let name = self.bump();
            let colon = self.significant(self.pos);
            let ty = if self.is_p(colon, ':') {
                self.pos = colon + 1;
                self.line_breaks();
                Some(self.type_expr(1, false)?)
            } else {
                None
            };
            names.push((name, ty));
            let comma = self.significant(self.pos);
            if !self.is_p(comma, ',') {
                break;
            }
            self.pos = comma + 1;
            self.line_breaks();
        }
        Ok(Property {
            keyword,
            names,
            span: Span {
                start,
                end: self.last_end(),
            },
        })
    }

    fn rescue_tail(&mut self) -> Result<Rescued> {
        let mut rescues = Vec::new();
        while self.at_word("rescue") {
            let keyword = self.bump();
            let line = self.tokens[keyword].line;
            let binding = self.rescue_clause(line)?;
            let existed = binding
                .as_ref()
                .is_some_and(|name| self.locals.contains(name));
            if let Some(name) = &binding {
                self.locals.insert(name.clone());
            }
            let body = self.block(&["rescue", "else", "ensure", "end"])?;
            if let Some(name) = &binding
                && !existed
            {
                self.locals.remove(name);
            }
            rescues.push(RescueClause {
                keyword,
                binding,
                body,
            });
        }
        let alternate = if self.take_word("else").is_some() {
            Some(self.block(&["ensure", "end"])?)
        } else {
            None
        };
        let ensure = if self.take_word("ensure").is_some() {
            Some(self.block(&["end"])?)
        } else {
            None
        };
        Ok(Rescued {
            rescues,
            alternate,
            ensure,
        })
    }

    fn rescue_clause(&mut self, line: usize) -> Result<Option<String>> {
        let next = self.significant(self.pos);
        if self.tokens[next].line != line {
            return Ok(None);
        }
        let grouped = self.is_p(next, '(');
        if grouped || self.ident(next) {
            self.pos = next;
            if grouped {
                self.bump();
                self.line_breaks();
            }
            loop {
                self.type_atom(1)?;
                let pipe = self.significant(self.pos);
                if !self.is_p(pipe, '|') {
                    break;
                }
                self.pos = pipe + 1;
                self.line_breaks();
            }
            if grouped {
                self.line_breaks();
                self.expect_p(')')?;
            }
        } else if !self.is_op(next, "=>") {
            return Ok(None);
        }
        let arrow = self.significant(self.pos);
        if self.tokens[arrow].line != line || !self.is_op(arrow, "=>") {
            return Ok(None);
        }
        let name = self.significant(arrow + 1);
        self.pos = name;
        if !self.ident(name) {
            return self.fail("rescue binding must be an identifier");
        }
        Ok(Some({
            let tok = self.bump();
            self.text(tok).to_owned()
        }))
    }

    // Expressions ---------------------------------------------------------

    fn trailing_block(&mut self, expr: Expr) -> Result<Expr> {
        let next = self.significant(self.pos);
        let brace = if self.is_word(next, "do") {
            (next != self.pos || self.can_attach_do()).then_some(false)
        } else if self.is_p(next, '{') {
            (next == self.pos && self.block_follows(&expr)).then_some(true)
        } else {
            None
        };
        let Some(brace) = brace else {
            return Ok(expr);
        };
        self.pos = next;
        self.block_expression(expr, brace)
    }

    fn block_line_expr(&mut self) -> Result<Expr> {
        let expr = self.line_expr(0)?;
        self.trailing_block(expr)
    }

    fn line_expr(&mut self, min: u8) -> Result<Expr> {
        self.line_exprs += 1;
        let result = self.expr(min);
        self.line_exprs -= 1;
        result
    }

    fn expr(&mut self, min: u8) -> Result<Expr> {
        let line = self.tokens[self.pos].line;
        let lhs = self.prefix_expr()?;
        self.expr_tail(lhs, min, None, Some(line), false)
    }

    fn prefix_expr(&mut self) -> Result<Expr> {
        self.nested(Self::unnested_prefix_expr)
    }

    fn unnested_prefix_expr(&mut self) -> Result<Expr> {
        let start = self.start();
        let open = self.pos;
        let tok = self.bump();
        let kind = self.kind_at(tok).clone();
        let expr = match kind {
            TokenKind::Word => {
                let word = self.text(tok);
                self.word_expression(word, tok)?
            }
            TokenKind::Punct('(') => {
                self.groups += 1;
                self.line_breaks();
                let inner = self.expr(0)?;
                self.line_breaks();
                let close = self.expect_p(')')?;
                self.groups -= 1;
                Expr {
                    span: Span {
                        start,
                        end: self.tokens[close].end,
                    },
                    kind: ExprKind::Group(open, Box::new(inner), close),
                }
            }
            TokenKind::Punct('[') => {
                let items = self.arguments(']', true)?;
                Expr {
                    span: Span {
                        start,
                        end: self.last_end(),
                    },
                    kind: ExprKind::Array(items),
                }
            }
            TokenKind::Punct('{') => self.hash_expr(start)?,
            TokenKind::Operator(op @ (".." | "...")) => {
                let _ = op;
                let next = self.significant(self.pos);
                if !self.prefix(next) {
                    return self.fail("range is missing end expression");
                }
                self.pos = next;
                let end = self.expr(8)?;
                Expr {
                    span: Span {
                        start,
                        end: end.span.end,
                    },
                    kind: ExprKind::Range(None, tok, Some(Box::new(end))),
                }
            }
            TokenKind::Operator("-" | "+" | "!") => {
                let literal =
                    self.kind_at(tok) == &TokenKind::Operator("-") && self.negative_literal(tok);
                let value = if literal {
                    self.prefix_expr()?
                } else {
                    self.line_breaks();
                    self.expr(13)?
                };
                Expr {
                    span: Span {
                        start,
                        end: value.span.end,
                    },
                    kind: ExprKind::Unary(tok, Box::new(value)),
                }
            }
            _ => self.leaf(tok)?,
        };
        Ok(expr)
    }

    fn leaf(&mut self, tok: Tok) -> Result<Expr> {
        let token = &self.tokens[tok];
        let span = Span {
            start: token.start,
            end: token.end,
        };
        let kind = match &token.kind {
            TokenKind::Integer => ExprKind::Integer,
            TokenKind::Float => ExprKind::Float,
            TokenKind::Regex => ExprKind::Regex,
            TokenKind::String(_) => ExprKind::Str,
            TokenKind::Template(spans) => {
                let spans = spans.clone();
                ExprKind::Template(
                    spans
                        .into_iter()
                        .map(|span| self.interpolation(span))
                        .collect(),
                )
            }
            TokenKind::Words { .. } => ExprKind::Words,
            TokenKind::Symbol { .. } => ExprKind::Symbol,
            _ => {
                self.pos = tok;
                return self.fail("unexpected token");
            }
        };
        Ok(Expr { span, kind })
    }

    /// Parses an interpolation's content as an expression of its own.
    /// The fragment's tokens join the tree's after its end, so every token
    /// index in the tree refers to one list.
    fn interpolation(&mut self, span: std::ops::Range<usize>) -> Option<Expr> {
        let text = &self.source[span.clone()];
        let tokens = lex(text, span.start).ok()?;
        let base = self.tokens.len();
        self.tokens.extend(tokens);
        let mut parser = Parser::new(self.source, std::mem::take(&mut self.tokens));
        parser.pos = base;
        parser.floor = base;
        parser.depth = self.depth;
        parser.limit = self.limit;
        parser.locals = self.locals.clone();
        parser.declared_it = self.declared_it;
        parser.type_names = self.type_names.clone();
        parser.lines();
        let expr = parser.line_expr(0).ok();
        parser.lines();
        let complete = parser.eof(parser.pos);
        self.too_deep = self.too_deep.or(parser.too_deep);
        self.tokens = parser.tokens;
        expr.filter(|_| complete)
    }

    fn word_expression(&mut self, word: &str, tok: Tok) -> Result<Expr> {
        let token = &self.tokens[tok];
        let span = Span {
            start: token.start,
            end: token.end,
        };
        let kind = match word {
            "nil" => ExprKind::Nil,
            "true" => ExprKind::True,
            "false" => ExprKind::False,
            "self" => ExprKind::SelfRef,
            "if" | "unless" => {
                let node = self.if_expr(tok, word == "unless")?;
                return Ok(Expr {
                    span: Span {
                        start: span.start,
                        end: self.last_end(),
                    },
                    kind: ExprKind::If(Box::new(node)),
                });
            }
            "case" => {
                let node = self.case_expr(tok)?;
                return Ok(Expr {
                    span: Span {
                        start: span.start,
                        end: self.last_end(),
                    },
                    kind: ExprKind::Case(Box::new(node)),
                });
            }
            "yield" => return self.yield_expr(tok),
            "begin" => return self.begin_expression(tok),
            "while" | "until" | "for" => {
                let kind = if word == "for" {
                    StmtKind::For(self.for_stmt()?)
                } else {
                    StmtKind::While(self.while_stmt(tok, word == "until")?)
                };
                let span = Span {
                    start: span.start,
                    end: self.last_end(),
                };
                return Ok(Expr {
                    span,
                    kind: ExprKind::Loop(Box::new(Stmt { span, kind })),
                });
            }
            _ if keyword(word) && word != "then" => {
                self.pos = tok;
                return self.fail("unexpected keyword");
            }
            _ if word.starts_with('@') => ExprKind::Ivar(word.to_owned()),
            _ => ExprKind::Name(word.to_owned()),
        };
        Ok(Expr { span, kind })
    }

    fn begin_expression(&mut self, keyword: Tok) -> Result<Expr> {
        let start = self.tokens[keyword].start;
        let body = self.block(&["rescue", "else", "ensure", "end"])?;
        let rescued = self.rescue_tail()?;
        let end = self.expect_word("end")?;
        Ok(Expr {
            span: Span {
                start,
                end: self.tokens[end].end,
            },
            kind: ExprKind::Begin(Box::new(Begin {
                keyword,
                body,
                rescued,
                end,
            })),
        })
    }

    fn case_expr(&mut self, keyword: Tok) -> Result<Case> {
        self.lines();
        let subject = if self.at_word("when") {
            None
        } else {
            Some(self.line_expr(0)?)
        };
        self.lines();
        let mut whens = Vec::new();
        while let Some(when) = self.take_word("when") {
            let mut values = Vec::new();
            loop {
                let splat = self.at_op("*");
                if splat {
                    self.bump();
                }
                values.push((self.condition()?, splat));
                let comma = self.significant(self.pos);
                if !self.is_p(comma, ',') {
                    break;
                }
                self.pos = comma + 1;
                self.lines();
            }
            self.take_word("then");
            self.lines();
            let result = self.expr(0)?;
            let result = self.trailing_block(result)?;
            whens.push(When {
                keyword: when,
                values,
                result,
            });
            self.lines();
        }
        if whens.is_empty() {
            return self.fail("expected when");
        }
        let alternate = if let Some(tok) = self.take_word("else") {
            self.lines();
            let alternate = self.expr(0)?;
            Some((tok, self.trailing_block(alternate)?))
        } else {
            None
        };
        self.lines();
        let end = self.expect_word("end")?;
        Ok(Case {
            keyword,
            subject,
            whens,
            alternate,
            end,
        })
    }

    fn yield_expr(&mut self, keyword: Tok) -> Result<Expr> {
        let start = self.tokens[keyword].start;
        let line = self.previous().line;
        let mut next = self.pos;
        while self.newline(next) {
            next += 1;
        }
        if self.is_p(next, '(') {
            self.pos = next;
        }
        let args = if let Some(open) = self.take_p('(') {
            let items = self.arguments(')', true)?;
            let close = self.pos - 1;
            Some(Args {
                parens: Some((open, close)),
                items: items
                    .into_iter()
                    .map(|value| Arg {
                        kind: ArgKind::Positional,
                        span: value.span,
                        value,
                    })
                    .collect(),
            })
        } else {
            let mut items = Vec::new();
            if self.tokens[self.pos].line == line && self.starts_expression() {
                items.push(self.line_expr(0)?);
                while self.at_p(',')
                    && self.tokens[self.pos].line == line
                    && self.tokens[self.pos + 1].line == line
                {
                    self.bump();
                    items.push(self.line_expr(0)?);
                }
            }
            (!items.is_empty()).then(|| Args {
                parens: None,
                items: items
                    .into_iter()
                    .map(|value| Arg {
                        kind: ArgKind::Positional,
                        span: value.span,
                        value,
                    })
                    .collect(),
            })
        };
        Ok(Expr {
            span: Span {
                start,
                end: self.last_end(),
            },
            kind: ExprKind::Yield(keyword, args),
        })
    }

    fn hash_expr(&mut self, start: usize) -> Result<Expr> {
        // `{` has been read; try the hash reading first, as the compiler does.
        let open = self.pos - 1;
        let saved = self.save();
        match self.hash_group(start) {
            Ok(hash) => Ok(hash),
            Err(error) => {
                self.restore(saved);
                self.pos = open;
                if self.type_expr(1, false).is_ok() {
                    Ok(Expr {
                        span: Span {
                            start,
                            end: self.last_end(),
                        },
                        kind: ExprKind::TypeLiteral,
                    })
                } else {
                    Err(error)
                }
            }
        }
    }

    fn hash_group(&mut self, start: usize) -> Result<Expr> {
        let mut entries = Vec::new();
        self.groups += 1;
        self.line_breaks();
        if self.take_p('}').is_none() {
            loop {
                let labeled = self.word_at(self.pos).is_some_and(|w| !w.starts_with('@'))
                    || matches!(self.kind(), TokenKind::String(_));
                let colon = self.significant(self.pos + 1);
                if !labeled || !self.is_p(colon, ':') {
                    return self.fail("invalid hash pair");
                }
                let key = self.bump();
                let name = match self.kind_at(key) {
                    TokenKind::String(bytes) => bytes.clone(),
                    _ => self.text(key).as_bytes().to_vec(),
                };
                let label = *self.kind_at(key) == TokenKind::Word;
                self.line_breaks();
                self.expect_p(':')?;
                self.line_breaks();
                let shorthand = matches!(self.kind(), TokenKind::Punct(',' | '}') | TokenKind::Eof);
                let value = if shorthand {
                    if !label {
                        return self.fail("missing value for hash key");
                    }
                    let token = &self.tokens[key];
                    Expr {
                        span: Span {
                            start: token.start,
                            end: token.end,
                        },
                        kind: ExprKind::Name(self.text(key).to_owned()),
                    }
                } else {
                    self.expr(0)?
                };
                entries.push(Entry {
                    key,
                    name,
                    shorthand,
                    value,
                });
                self.line_breaks();
                if self.take_p('}').is_some() {
                    break;
                }
                if !self.at_p(',') {
                    return self.fail("invalid hash pair");
                }
                self.bump();
                self.line_breaks();
                if self.take_p('}').is_some() {
                    break;
                }
            }
        }
        self.groups -= 1;
        Ok(Expr {
            span: Span {
                start,
                end: self.last_end(),
            },
            kind: ExprKind::Hash(entries),
        })
    }

    fn expr_tail(
        &mut self,
        mut lhs: Expr,
        min: u8,
        mut next: Option<Suffix>,
        mut line: Option<usize>,
        unlimited: bool,
    ) -> Result<Expr> {
        loop {
            let suffix = match next.take() {
                Some(suffix) => Some(suffix),
                None if unlimited => self.unlimited_suffix(&lhs, min),
                None => self.expression_suffix(&lhs, min, line.take()),
            };
            let Some(suffix) = suffix else {
                return Ok(lhs);
            };
            lhs = match suffix {
                Suffix::Rescue => {
                    let keyword = self.pos - 1;
                    let next = self.significant(self.pos);
                    if self.tokens[next].line != self.tokens[keyword].line || !self.prefix(next) {
                        return self.fail("rescue modifier requires fallback expression");
                    }
                    let fallback = self.line_expr(0)?;
                    Expr {
                        span: Span {
                            start: lhs.span.start,
                            end: fallback.span.end,
                        },
                        kind: ExprKind::Rescue(Box::new(lhs), keyword, Box::new(fallback)),
                    }
                }
                Suffix::Command => self.command_expression(lhs)?,
                Suffix::Block(brace) => self.block_expression(lhs, brace)?,
                Suffix::Call => {
                    let open = self.pos - 1;
                    let items = self.call_arguments(false)?;
                    let close = self.pos - 1;
                    self.parenthesized_call(
                        lhs,
                        Args {
                            parens: Some((open, close)),
                            items,
                        },
                    )
                }
                Suffix::Scope => self.scoped_expression(lhs)?,
                Suffix::Member => self.member_expression(lhs)?,
                Suffix::Index => {
                    let open = self.pos - 1;
                    let close_at = self.significant(self.pos);
                    if self.is_p(close_at, ']') {
                        return self.fail("index expression requires at least one selector");
                    }
                    let items = self.arguments(']', false)?;
                    let close = self.pos - 1;
                    Expr {
                        span: Span {
                            start: lhs.span.start,
                            end: self.tokens[close].end,
                        },
                        kind: ExprKind::Index(Box::new(lhs), open, items, close),
                    }
                }
                Suffix::Ternary => self.ternary_expression(lhs)?,
                Suffix::Binary(right) => self.binary_expression(lhs, right)?,
            };
            line = Some(self.previous().line);
        }
    }

    fn unlimited_suffix(&mut self, lhs: &Expr, min: u8) -> Option<Suffix> {
        let (line_exprs, groups) = (self.line_exprs, self.groups);
        self.line_exprs = 0;
        self.groups += 1;
        let suffix = self.expression_suffix(lhs, min, None);
        (self.line_exprs, self.groups) = (line_exprs, groups);
        suffix
    }

    fn begin_call(&self, lhs: &Expr) -> bool {
        // The compiler's parser keeps no node for parentheses, so a
        // parenthesized `begin` calls as the bare one does.
        self.at_p('(') && matches!(ungrouped(lhs).kind, ExprKind::Begin(_))
    }

    fn expression_suffix(&mut self, lhs: &Expr, min: u8, line: Option<usize>) -> Option<Suffix> {
        let resumed = self.continuation_position(min);
        if let Some(next) = resumed {
            self.pos = next;
        } else if line.is_some_and(|line| self.tokens[self.pos].line > line)
            && self.line_exprs > 0
            && !self.end_line(self.pos)
            && !self.limit_continues(self.pos)
            && !self.begin_call(lhs)
        {
            return None;
        }
        if min == 0
            && (self.command_depth == 0 || self.groups > self.command_group)
            && (resumed.is_some() || self.tokens[self.pos].line == self.previous().end_line)
            && self.at_word("rescue")
            && !self.keyword_label(self.pos)
        {
            self.pos += 1;
            return Some(Suffix::Rescue);
        }
        if self.command_start(lhs, min) {
            return Some(Suffix::Command);
        }
        if self.at_p('{')
            && self.tokens[self.pos].line == self.previous().end_line
            && self.block_follows(lhs)
        {
            return Some(Suffix::Block(true));
        }
        if self.at_word("do")
            && (self.can_attach_do()
                || (self.pos != self.call_end && self.significant(self.call_end) == self.pos))
        {
            return Some(Suffix::Block(false));
        }
        if self.take_p('(').is_some() {
            return Some(Suffix::Call);
        }
        if self.at_op("::") {
            return Some(Suffix::Scope);
        }
        if self.at_op("&.") || self.at_p('.') {
            return Some(Suffix::Member);
        }
        if self.take_p('[').is_some() {
            return Some(Suffix::Index);
        }
        if min <= 2 && self.take_p('?').is_some() {
            return Some(Suffix::Ternary);
        }
        let TokenKind::Operator(op) = self.kind() else {
            return None;
        };
        let (left, right) = binding_power(op)?;
        (left >= min).then_some(Suffix::Binary(right))
    }

    fn command_expression(&mut self, lhs: Expr) -> Result<Expr> {
        self.command_depth += 1;
        let group = std::mem::replace(&mut self.command_group, self.groups);
        let items = self.command_arguments();
        self.command_group = group;
        self.command_depth -= 1;
        let items = items?;
        let args = Args {
            parens: None,
            items,
        };
        let span = Span {
            start: lhs.span.start,
            end: self.last_end(),
        };
        let lhs = ungroup(lhs);
        let call = match lhs.kind {
            ExprKind::Name(name) => {
                let name_tok = self.token_at(lhs.span.start);
                Call {
                    receiver: None,
                    operator: None,
                    name,
                    name_tok,
                    args: Some(args),
                    block: None,
                }
            }
            ExprKind::Call(mut call) => {
                call.args = Some(args);
                *call
            }
            _ => unreachable!(),
        };
        Ok(Expr {
            span,
            kind: ExprKind::Call(Box::new(call)),
        })
    }

    fn token_at(&self, offset: usize) -> Tok {
        token_at(&self.tokens, offset)
    }

    fn command_arguments(&mut self) -> Result<Vec<Arg>> {
        let mut items = Vec::new();
        loop {
            items.push(self.call_argument(false, false)?);
            let last = self.previous().line;
            if !self.at_p(',')
                || self.tokens[self.pos].line != last
                || self.tokens[self.pos + 1].line != last
                || !self.command_argument_start(self.pos + 1, true)
            {
                break;
            }
            self.bump();
        }
        Ok(items)
    }

    fn arguments(&mut self, close: char, trailing: bool) -> Result<Vec<Expr>> {
        let mut items = Vec::new();
        self.groups += 1;
        self.line_breaks();
        if self.take_p(close).is_some() {
            self.groups -= 1;
            return Ok(items);
        }
        loop {
            items.push(self.expr(0)?);
            self.line_breaks();
            if self.take_p(close).is_some() {
                break;
            }
            if !self.at_p(',') {
                return self.fail(&format!("expected {close}"));
            }
            self.bump();
            self.line_breaks();
            if trailing && self.take_p(close).is_some() {
                break;
            }
        }
        self.groups -= 1;
        Ok(items)
    }

    fn call_arguments(&mut self, types: bool) -> Result<Vec<Arg>> {
        let mut items = Vec::new();
        self.groups += 1;
        self.line_breaks();
        if self.take_p(')').is_some() {
            self.groups -= 1;
            return Ok(items);
        }
        loop {
            items.push(self.call_argument(true, types)?);
            self.line_breaks();
            if self.take_p(')').is_some() {
                break;
            }
            if !self.at_p(',') {
                return self.fail("expected )");
            }
            self.bump();
            self.line_breaks();
            if self.take_p(')').is_some() {
                break;
            }
        }
        self.groups -= 1;
        self.call_end = self.pos;
        Ok(items)
    }

    fn call_argument(&mut self, parenthesized: bool, types: bool) -> Result<Arg> {
        let start = self.start();
        if self.at_op("&") {
            return self.fail("block arguments are not supported");
        }
        let kind = if self.at_op("**") {
            self.bump();
            ArgKind::KeywordSplat
        } else if self.keyword_label(self.pos) {
            let name = {
                let tok = self.bump();
                self.text(tok).to_owned()
            };
            self.bump();
            ArgKind::Keyword(name)
        } else if self.at_op("*") {
            self.bump();
            ArgKind::Splat
        } else {
            ArgKind::Positional
        };
        if parenthesized || matches!(kind, ArgKind::Splat | ArgKind::KeywordSplat) {
            self.line_breaks();
        }
        if let ArgKind::Keyword(name) = &kind {
            let shorthand = self.at_p(',')
                || (parenthesized && self.at_p(')'))
                || (!parenthesized
                    && (self.eof(self.pos)
                        || self.newline(self.pos)
                        || self.tokens[self.pos].line != self.tokens[self.pos - 1].end_line));
            if shorthand {
                let label = self.pos - 2;
                let token = &self.tokens[label];
                let value = Expr {
                    span: Span {
                        start: token.start,
                        end: token.end,
                    },
                    kind: ExprKind::Name(name.clone()),
                };
                return Ok(Arg {
                    kind,
                    span: Span {
                        start,
                        end: self.last_end(),
                    },
                    value,
                });
            }
        } else if parenthesized
            && kind == ArgKind::Positional
            && let Some(value) = self.argument_type_literal(types)
        {
            return Ok(Arg {
                kind,
                span: value.span,
                value,
            });
        }
        let value = self.expr(0)?;
        Ok(Arg {
            kind,
            span: Span {
                start,
                end: value.span.end,
            },
            value,
        })
    }

    /// Reads a builtin type literal passed as a parenthesized argument, such
    /// as the `array<int>` of `JSON.parse_as(text, array<int>)`.
    /// In a call that takes types, as `as` and `JSON.parse_as` do, a tuple
    /// type such as `[string, hash<string, int>]` is one too.
    fn argument_type_literal(&mut self, types: bool) -> Option<Expr> {
        if self.word_at(self.pos).is_none() && !(types && self.at_p('[')) {
            return None;
        }
        let start = self.pos;
        let saved = self.save();
        let candidate = self.type_expr(1, false);
        let end = self.pos;
        self.line_breaks();
        let boundary = matches!(self.kind(), TokenKind::Punct(',' | ')'));
        self.restore(saved);
        let ty = candidate.ok()?;
        let nil = matches!(&ty.kind, TypeKind::Named(tok, _) if self.text(*tok) == "nil");
        // A bare scoped name is the class's own value, as in the compiler.
        let scoped = matches!(ty.kind, TypeKind::Qualified(_)) && !ty.nullable;
        if !boundary
            || nil
            || scoped
            || matches!(ty.kind, TypeKind::Shape(..))
            || !self.builtin_leaves(&ty)
        {
            return None;
        }
        self.pos = end;
        let span = Span {
            start: self.tokens[start].start,
            end: self.tokens[end - 1].end,
        };
        Some(if end == start + 1 {
            Expr {
                span,
                kind: ExprKind::Name(self.text(start).to_owned()),
            }
        } else {
            Expr {
                span,
                kind: ExprKind::TypeLiteral,
            }
        })
    }

    /// Whether every name in a type argument is one [`Self::type_name`]
    /// accepts, as the compiler reads a cast's type.
    fn builtin_leaves(&self, ty: &TypeExpr) -> bool {
        match &ty.kind {
            TypeKind::Named(tok, args) => {
                self.type_name(self.text(*tok).trim_end_matches('?'))
                    && args.iter().all(|arg| self.builtin_leaves(arg))
            }
            TypeKind::Qualified(names) => {
                let last = self.text(*names.last().unwrap()).trim_end_matches('?');
                self.is_op(names[0] + 1, "::") && self.type_names.contains(last)
            }
            TypeKind::Shape(fields, _) => fields.iter().all(|(_, ty)| self.builtin_leaves(ty)),
            TypeKind::Union(options) | TypeKind::Tuple(options) => {
                options.iter().all(|ty| self.builtin_leaves(ty))
            }
        }
    }

    fn parenthesized_call(&mut self, lhs: Expr, args: Args) -> Expr {
        let span = Span {
            start: lhs.span.start,
            end: self.last_end(),
        };
        let lhs = ungroup(lhs);
        let kind = match lhs.kind {
            ExprKind::Name(name) => {
                let name_tok = self.token_at(lhs.span.start);
                ExprKind::Call(Box::new(Call {
                    receiver: None,
                    operator: None,
                    name,
                    name_tok,
                    args: Some(args),
                    block: None,
                }))
            }
            ExprKind::Call(mut call) if call.args.is_none() && call.block.is_none() => {
                call.args = Some(args);
                ExprKind::Call(call)
            }
            kind => ExprKind::Computed(
                Box::new(Expr {
                    span: lhs.span,
                    kind,
                }),
                args,
            ),
        };
        Expr { span, kind }
    }

    fn block_expression(&mut self, lhs: Expr, brace: bool) -> Result<Expr> {
        let block = self.attached_block(brace)?;
        let span = Span {
            start: lhs.span.start,
            end: self.tokens[block.close].end,
        };
        let lhs = ungroup(lhs);
        let kind = match lhs.kind {
            ExprKind::Name(name) => {
                let name_tok = self.token_at(lhs.span.start);
                ExprKind::Call(Box::new(Call {
                    receiver: None,
                    operator: None,
                    name,
                    name_tok,
                    args: None,
                    block: Some(block),
                }))
            }
            ExprKind::Call(mut call) => {
                // A second block replaces the first, as in the compiler.
                call.block = Some(block);
                ExprKind::Call(call)
            }
            ExprKind::BlockCall(callee, _) => ExprKind::BlockCall(callee, Box::new(block)),
            kind => ExprKind::BlockCall(
                Box::new(Expr {
                    span: lhs.span,
                    kind,
                }),
                Box::new(block),
            ),
        };
        Ok(Expr { span, kind })
    }

    fn scoped_expression(&mut self, lhs: Expr) -> Result<Expr> {
        let operator = self.bump();
        self.line_breaks();
        if !self.ident(self.pos) && !self.at_word("enum") {
            return self.fail("expected identifier");
        }
        let name_tok = self.bump();
        let name = self.text(name_tok).to_owned();
        let args = if let Some(open) = self.take_p('(') {
            let items = self.call_arguments(false)?;
            Some(Args {
                parens: Some((open, self.pos - 1)),
                items,
            })
        } else {
            None
        };
        Ok(Expr {
            span: Span {
                start: lhs.span.start,
                end: self.last_end(),
            },
            kind: ExprKind::Call(Box::new(Call {
                receiver: Some(lhs),
                operator: Some(operator),
                name,
                name_tok,
                args,
                block: None,
            })),
        })
    }

    fn member_expression(&mut self, lhs: Expr) -> Result<Expr> {
        let operator = self.bump();
        self.line_breaks();
        let name_tok = match self.kind() {
            TokenKind::Word if !self.text(self.pos).starts_with('@') => self.bump(),
            TokenKind::Operator("<=>") => self.bump(),
            _ => return self.fail("expected member name"),
        };
        let name = self.text(name_tok).to_owned();
        let args = if let Some(open) = self.take_p('(') {
            // As in the compiler, `as` and `JSON.parse_as` take types.
            let types = name == "as"
                || (name == "parse_as"
                    && matches!(&ungrouped(&lhs).kind, ExprKind::Name(r) if r == "JSON"));
            let items = self.call_arguments(types)?;
            Some(Args {
                parens: Some((open, self.pos - 1)),
                items,
            })
        } else {
            None
        };
        Ok(Expr {
            span: Span {
                start: lhs.span.start,
                end: self.last_end(),
            },
            kind: ExprKind::Call(Box::new(Call {
                receiver: Some(lhs),
                operator: Some(operator),
                name,
                name_tok,
                args,
                block: None,
            })),
        })
    }

    fn attached_block(&mut self, brace: bool) -> Result<Block> {
        let open = self.bump();
        self.line_breaks();
        let outer = self.locals.clone();
        let outer_it = self.declared_it;
        let (params, pipes) = self.block_parameters()?;
        let previous_loop = self.loop_condition.take();
        let previous_then = self.then_stop.take();
        let command_depth = std::mem::replace(&mut self.command_depth, 0);
        let body = self.block(if brace { &["}"] } else { &["end"] });
        self.command_depth = command_depth;
        self.then_stop = previous_then;
        self.loop_condition = previous_loop;
        let body = body?;
        let close = if brace {
            self.expect_p('}')?
        } else {
            self.expect_word("end")?
        };
        self.locals = outer;
        self.declared_it = outer_it;
        Ok(Block {
            open,
            brace,
            params,
            pipes,
            body,
            close,
        })
    }

    fn block_parameters(&mut self) -> Result<(Vec<Target>, Pipes)> {
        let mut params = Vec::new();
        let pipes = if self.at_op("||") {
            let tok = self.bump();
            Some((tok, tok))
        } else if let Some(open) = self.take_p('|') {
            self.line_breaks();
            if let Some(close) = self.take_p('|') {
                Some((open, close))
            } else {
                loop {
                    let target = self.block_parameter()?;
                    self.declare_target(&target);
                    params.push(target);
                    let comma = self.significant(self.pos);
                    if !self.is_p(comma, ',') {
                        break;
                    }
                    self.pos = comma + 1;
                    self.line_breaks();
                }
                self.line_breaks();
                let close = self.expect_p('|')?;
                Some((open, close))
            }
        } else {
            None
        };
        if pipes.is_none() {
            self.locals.insert("it".to_owned());
            for n in ["_1", "_2", "_3", "_4", "_5", "_6", "_7", "_8", "_9"] {
                self.locals.insert(n.to_owned());
            }
        }
        Ok((params, pipes))
    }

    fn block_parameter(&mut self) -> Result<Target> {
        let close = match self.kind() {
            TokenKind::Punct('(') => ')',
            TokenKind::Punct('[') => ']',
            _ if self.ident(self.pos) => {
                let tok = self.bump();
                let token = &self.tokens[tok];
                let target = Target::Expr(Expr {
                    span: Span {
                        start: token.start,
                        end: token.end,
                    },
                    kind: ExprKind::Name(self.text(tok).to_owned()),
                });
                let colon = self.significant(self.pos);
                if !self.is_p(colon, ':') {
                    return Ok(target);
                }
                self.pos = colon + 1;
                self.line_breaks();
                let ty = self.type_expr(0, true)?;
                return Ok(Target::Typed(Box::new(target), ty));
            }
            _ => return self.fail("expected block parameter"),
        };
        let start = self.start();
        self.bump();
        self.line_breaks();
        let (inner, tuple) = self.target(Place::Group(true))?;
        self.line_breaks();
        self.expect_p(close)?;
        let parts = match inner {
            Target::Group(_, parts) if tuple => parts,
            inner => vec![inner],
        };
        Ok(Target::Group(
            Span {
                start,
                end: self.last_end(),
            },
            parts,
        ))
    }

    fn ternary_expression(&mut self, condition: Expr) -> Result<Expr> {
        let question = self.pos - 1;
        self.lines();
        self.ternaries.push(self.groups);
        let yes = self.expr(0)?;
        self.ternaries.pop();
        self.ternary_separator();
        let colon = self.expect_p(':')?;
        let _ = colon;
        self.lines();
        let no = self.expr(2)?;
        Ok(Expr {
            span: Span {
                start: condition.span.start,
                end: no.span.end,
            },
            kind: ExprKind::Ternary(Box::new(condition), question, Box::new(yes), Box::new(no)),
        })
    }

    fn binary_expression(&mut self, lhs: Expr, right: u8) -> Result<Expr> {
        let op = self.bump();
        if matches!(self.kind_at(op), TokenKind::Operator(".." | "...")) {
            if self.groups > 0 {
                self.lines();
            }
            let stop = self.then_stop.is_some() && self.at_word("then");
            let end = if self.starts_expression() && !stop {
                Some(Box::new(self.expr(right)?))
            } else {
                None
            };
            let span = Span {
                start: lhs.span.start,
                end: end.as_ref().map_or(self.tokens[op].end, |end| end.span.end),
            };
            return Ok(Expr {
                span,
                kind: ExprKind::Range(Some(Box::new(lhs)), op, end),
            });
        }
        self.line_breaks();
        let rhs = self.expr(right)?;
        Ok(Expr {
            span: Span {
                start: lhs.span.start,
                end: rhs.span.end,
            },
            kind: ExprKind::Binary(op, Box::new(lhs), Box::new(rhs)),
        })
    }

    // Types ---------------------------------------------------------------

    fn type_expr(&mut self, depth: usize, block: bool) -> Result<TypeExpr> {
        if depth > 64 {
            return self.fail("type annotation nesting too deep");
        }
        self.line_breaks();
        let first = self.type_atom(depth)?;
        let mut options = vec![first];
        loop {
            let boundary = self.pos;
            self.line_breaks();
            if !self.at_p('|') || (block && !self.block_type_continues(depth)) {
                self.pos = boundary;
                break;
            }
            self.bump();
            self.line_breaks();
            options.push(self.type_atom(depth)?);
        }
        if options.len() == 1 {
            return Ok(options.pop().unwrap());
        }
        let span = Span {
            start: options[0].span.start,
            end: options.last().unwrap().span.end,
        };
        Ok(TypeExpr {
            span,
            kind: TypeKind::Union(options),
            nullable: false,
        })
    }

    fn block_type_continues(&mut self, depth: usize) -> bool {
        let saved = self.pos;
        self.bump();
        self.line_breaks();
        let result = self.type_atom(depth).is_ok()
            && matches!(
                self.kind_at(self.significant(self.pos)),
                TokenKind::Punct(',' | '|')
            );
        self.pos = saved;
        result
    }

    fn type_atom(&mut self, depth: usize) -> Result<TypeExpr> {
        let start = self.start();
        // As in the compiler's parser, a bracket in a type is always a
        // tuple, whose elements may be shapes or tuples themselves.
        let mut ty = if self.take_p('{').is_some() {
            self.type_shape(depth, start)?
        } else if self.at_p('[') {
            self.bump();
            let mut elements = Vec::new();
            loop {
                self.line_breaks();
                elements.push(self.type_expr(depth + 1, false)?);
                self.line_breaks();
                if self.take_p(']').is_some() {
                    break;
                }
                self.expect_p(',')?;
            }
            TypeExpr {
                span: Span {
                    start,
                    end: self.last_end(),
                },
                kind: TypeKind::Tuple(elements),
                nullable: false,
            }
        } else {
            self.named_type(depth)?
        };
        let question = self.significant(self.pos);
        if self.is_p(question, '?') {
            if ty.nullable {
                return self.fail("duplicate nullable suffix");
            }
            self.pos = question + 1;
            ty.nullable = true;
            ty.span.end = self.tokens[question].end;
        }
        Ok(ty)
    }

    fn named_type(&mut self, depth: usize) -> Result<TypeExpr> {
        if !self.ident(self.pos) && !self.at_word("nil") {
            return self.fail("expected type name");
        }
        let start = self.start();
        let tok = self.bump();
        let written = self.text(tok);
        let nullable = written.ends_with('?');
        // As in the compiler, a builtin type is not a namespace, however
        // it is spelled: only a declared type scopes through `::` or `.`.
        // A builtin's name folds in any case, and ADR-007's names — such
        // as a class spelled `Error` — keep naming the class.
        let builtin = crate::types::builtin_name(written.trim_end_matches('?')).is_some();
        let scope = self.significant(self.pos);
        if !builtin && self.is_op(scope, "::") && !nullable {
            // A nested class or module, `Outer::Inner`.
            let mut names = vec![tok];
            loop {
                let scope = self.significant(self.pos);
                if !self.is_op(scope, "::") || self.text(*names.last().unwrap()).ends_with('?') {
                    break;
                }
                self.pos = self.significant(scope + 1);
                if !self.ident(self.pos) {
                    return self.fail("expected identifier");
                }
                names.push(self.bump());
            }
            let last = *names.last().unwrap();
            return Ok(TypeExpr {
                span: Span {
                    start,
                    end: self.tokens[last].end,
                },
                nullable: self.text(last).ends_with('?'),
                kind: TypeKind::Qualified(names),
            });
        }
        let dot = self.significant(self.pos);
        if !builtin && self.is_p(dot, '.') && !nullable {
            self.pos = self.significant(dot + 1);
            if !self.ident(self.pos) {
                return self.fail("expected identifier");
            }
            let member = self.bump();
            return Ok(TypeExpr {
                span: Span {
                    start,
                    end: self.tokens[member].end,
                },
                kind: TypeKind::Qualified(vec![tok, member]),
                nullable: self.text(member).ends_with('?'),
            });
        }
        let open = self.significant(self.pos);
        if !self.is_op(open, "<") {
            return Ok(TypeExpr {
                span: Span {
                    start,
                    end: self.tokens[tok].end,
                },
                kind: TypeKind::Named(tok, Vec::new()),
                nullable,
            });
        }
        // As in the compiler, a container's name folds in any case, so only
        // `array`, `hash` and `object`, or the `type` of a type literal,
        // takes type arguments. The name is untrimmed on purpose: a
        // nullable `arraY?<int>` fails here, as the compiler refuses it.
        if !crate::types::builtin_name(written)
            .is_some_and(crate::types::BuiltinName::takes_type_arguments)
        {
            return self.fail("type does not accept type arguments");
        }
        self.pos = open + 1;
        let mut arguments = Vec::new();
        loop {
            self.line_breaks();
            arguments.push(self.type_expr(depth + 1, false)?);
            self.pos = self.significant(self.pos);
            if self.take_p(',').is_some() {
                continue;
            }
            if !self.at_op(">") {
                return self.fail("expected >");
            }
            self.bump();
            break;
        }
        Ok(TypeExpr {
            span: Span {
                start,
                end: self.last_end(),
            },
            kind: TypeKind::Named(tok, arguments),
            nullable: false,
        })
    }

    fn type_shape(&mut self, depth: usize, start: usize) -> Result<TypeExpr> {
        let mut fields = Vec::new();
        let mut open = false;
        self.line_breaks();
        if self.take_p('}').is_none() {
            loop {
                if self.at_op("...") {
                    self.bump();
                    self.line_breaks();
                    self.expect_p('}')?;
                    open = true;
                    break;
                }
                let name = match self.kind() {
                    TokenKind::Word if !self.text(self.pos).starts_with('@') => self.bump(),
                    TokenKind::String(_) | TokenKind::Symbol { .. } => self.bump(),
                    _ => return self.fail("expected shape field name"),
                };
                self.line_breaks();
                self.expect_p(':')?;
                self.line_breaks();
                let ty = self.type_expr(depth + 1, false)?;
                fields.push((name, ty));
                self.line_breaks();
                if self.take_p('}').is_some() {
                    break;
                }
                if self.take_p(',').is_none() {
                    return self.fail("expected }");
                }
                self.line_breaks();
            }
        }
        Ok(TypeExpr {
            span: Span {
                start,
                end: self.last_end(),
            },
            kind: TypeKind::Shape(fields, open),
            nullable: false,
        })
    }
}

/// Whether a lowercase type name is one ADR-004 spelled in any case, and
/// ADR-008 spells in lowercase only.
pub fn respelled_type(name: &str) -> bool {
    matches!(
        name,
        "any"
            | "int"
            | "float"
            | "number"
            | "string"
            | "symbol"
            | "bool"
            | "nil"
            | "duration"
            | "time"
            | "money"
            | "range"
            | "array"
            | "hash"
            | "object"
    )
}

/// The token that starts at `offset`.
fn start_token(parser: &Parser<'_>, offset: usize) -> Tok {
    parser.token_at(offset)
}

/// The token that starts at `offset`. The source's tokens are sorted, and
/// each interpolation's follow them.
pub fn token_at(tokens: &[Token], offset: usize) -> Tok {
    let end = tokens
        .iter()
        .position(|token| token.kind == TokenKind::Eof)
        .map_or(tokens.len(), |eof| eof + 1);
    let index = tokens[..end].partition_point(|token| token.start < offset);
    if index < end && tokens[index].start == offset {
        return index;
    }
    tokens[end..]
        .iter()
        .position(|token| token.start == offset && token.kind != TokenKind::Eof)
        .map_or(index.min(tokens.len() - 1), |found| end + found)
}
