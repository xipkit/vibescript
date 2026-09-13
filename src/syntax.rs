use crate::{Error, Result, Value};
use std::collections::HashSet;

mod lexer;
mod tokens;
mod types;
pub(crate) mod unicode;
use lexer::{Lexeme, Part, Token, lex};
use tokens::Tokens;

const MAX_DEPTH: usize = 128;
const MAX_SOURCE: usize = 8 << 20;

#[derive(Debug)]
pub(crate) struct Expr {
    pub node: Node,
    depth: usize,
}
#[derive(Debug)]
pub(crate) enum Node {
    Shape(Box<crate::types::Type>, Option<Box<Expr>>, Vec<String>),
    Integer(u64),
    BigInteger(String, u32),
    Literal(Value),
    Template(Vec<Expr>, bool),
    Var(String),
    Array(Vec<Expr>),
    Hash(Vec<(Vec<u8>, Expr)>),
    Unary(&'static str, Box<Expr>),
    Binary(&'static str, Box<Expr>, Box<Expr>),
    Range(Option<Box<Expr>>, Option<Box<Expr>>, bool),
    Conditional(Box<Expr>, Box<Expr>, Box<Expr>),
    Case(Option<Box<Expr>>, Vec<When>, Option<Box<Expr>>),
    Loop(Box<Stmt>),
    Call(String, Vec<Argument>),
    BlockCall(Box<Expr>, Block),
    Yield(Vec<Expr>),
    Member(Box<Expr>, String),
    Scope(Box<Expr>, String, Option<Vec<Argument>>),
    Method(Box<Expr>, String, Vec<Argument>),
    Index(Box<Expr>, Vec<Expr>),
}
#[derive(Debug)]
pub(crate) struct Block {
    pub params: Vec<Target>,
    pub body: Vec<Stmt>,
    pub implicit: bool,
    pub infer_it: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamKind {
    Positional,
    Keyword,
    Rest,
    KeywordRest,
}
#[derive(Debug)]
pub(crate) struct Parameter {
    pub name: String,
    pub kind: ParamKind,
    pub default: Option<Expr>,
    pub ty: Option<crate::types::Type>,
}
#[derive(Debug)]
pub(crate) enum ArgumentKind {
    Positional,
    Splat,
    Keyword(String),
    KeywordSplat,
}
#[derive(Debug)]
pub(crate) struct Argument {
    pub kind: ArgumentKind,
    pub value: Expr,
}
#[derive(Debug)]
pub(crate) struct When {
    pub values: Vec<(Expr, bool)>,
    pub result: Expr,
}
#[derive(Debug)]
pub(crate) enum Target {
    Value(Expr),
    Tuple(Vec<(Option<Target>, bool)>),
    Typed(Box<Target>, crate::types::Type),
}
impl Target {
    fn is_binding(&self) -> bool {
        match self {
            Self::Typed(target, _) => target.is_binding(),
            Self::Value(e) => matches!(e.node, Node::Var(_)),
            Self::Tuple(parts) => parts
                .iter()
                .all(|(target, _)| target.as_ref().is_none_or(Self::is_binding)),
        }
    }
    fn depth(&self) -> usize {
        match self {
            Self::Typed(target, _) => target.depth(),
            Self::Value(e) => e.depth,
            Self::Tuple(parts) => {
                1 + parts
                    .iter()
                    .filter_map(|(t, _)| t.as_ref())
                    .map(Self::depth)
                    .max()
                    .unwrap_or(0)
            }
        }
    }
}
#[derive(Debug)]
pub(crate) enum Stmt {
    Expr(Expr),
    Assign(Target, &'static str, Expr),
    If(Expr, Vec<Stmt>, Vec<Stmt>),
    While(Expr, Vec<Stmt>),
    For(Target, Expr, Vec<Stmt>),
    Return(Option<Expr>),
    Break(Option<Expr>),
    Next(Option<Expr>),
}
impl Stmt {
    fn depth(&self) -> usize {
        let body = |s: &[Stmt]| s.iter().map(Self::depth).max().unwrap_or(0);
        1 + match self {
            Self::Expr(e) => e.depth,
            Self::Assign(t, _, e) => t.depth().max(e.depth),
            Self::If(e, yes, no) => e.depth.max(body(yes)).max(body(no)),
            Self::While(e, b) => e.depth.max(body(b)),
            Self::For(t, e, b) => t.depth().max(e.depth).max(body(b)),
            Self::Return(e) | Self::Break(e) | Self::Next(e) => e.as_ref().map_or(0, |e| e.depth),
        }
    }
}
pub(crate) struct Definition {
    pub name: String,
    pub params: Vec<Parameter>,
    pub body: Vec<Stmt>,
    pub return_type: Option<crate::types::Type>,
}

pub(crate) struct Declarations {
    pub functions: Vec<Definition>,
    pub enums: Vec<(String, Vec<String>)>,
}

pub(crate) fn parse(source: &str) -> Result<Declarations> {
    let mut p = Parser {
        source,
        lex_depth: 0,
        tokens: Tokens::new(lex(source)?),
        pos: 0,
        depth: 0,
        groups: 0,
        line_exprs: 0,
        command_depth: 0,
        ternaries: Vec::new(),
        command_group: 0,
        loop_condition: None,
        locals: HashSet::new(),
        declared_it: false,
        type_structural_error: false,
    };
    let mut defs = Vec::new();
    let mut enums = Vec::new();
    let mut top = Vec::new();
    p.lines();
    while !matches!(p.token(), Token::Eof) {
        if p.word("def") {
            let name = p.name()?;
            let outer_locals = std::mem::take(&mut p.locals);
            let outer_it = std::mem::replace(&mut p.declared_it, false);
            let parenthesized = p.take_p('(');
            let params = p.parameters(parenthesized)?;
            p.line_breaks();
            let return_type = if p.token() == &Token::Op("->") {
                p.bump();
                Some(p.type_expr(1, false)?)
            } else {
                None
            };
            p.lines();
            let body = p.block(&["end"])?;
            p.expect_word("end")?;
            p.locals = outer_locals;
            p.declared_it = outer_it;
            if defs.iter().any(|d: &Definition| d.name == name) || name == "__main__" {
                return p.err("duplicate or reserved function name");
            }
            defs.push(Definition {
                name,
                params,
                body,
                return_type,
            });
        } else if p.word("enum") {
            p.line_breaks();
            let name = p.enum_name()?;
            let mut members = Vec::new();
            let mut seen = HashSet::new();
            p.lines();
            while !matches!(p.token(), Token::Eof)
                && !matches!(p.token(), Token::Word(w) if w == "end")
            {
                let member = if p.word("enum") {
                    "enum".to_owned()
                } else {
                    p.enum_name()?
                };
                if !seen.insert(member.clone()) {
                    return p.err("duplicate enum member");
                }
                members.push(member);
                p.lines();
            }
            if members.is_empty() {
                return p.err("enum must define at least one member");
            }
            p.expect_word("end")?;
            enums.push((name, members));
        } else {
            top.push(p.statement()?);
        }
        p.lines();
    }
    defs.insert(
        0,
        Definition {
            name: "__main__".into(),
            params: Vec::new(),
            body: top,
            return_type: None,
        },
    );
    Ok(Declarations {
        functions: defs,
        enums,
    })
}

struct Parser<'a> {
    source: &'a str,
    lex_depth: usize,
    tokens: Tokens,
    pos: usize,
    depth: usize,
    groups: usize,
    line_exprs: usize,
    command_depth: usize,
    ternaries: Vec<usize>,
    command_group: usize,
    loop_condition: Option<usize>,
    locals: HashSet<String>,
    declared_it: bool,
    type_structural_error: bool,
}
impl Parser<'_> {
    fn parameters(&mut self, parenthesized: bool) -> Result<Vec<Parameter>> {
        let mut params = Vec::new();
        let mut rest = false;
        let mut keywords = false;
        let mut keyword_rest = false;
        if parenthesized {
            self.groups += 1;
            self.lines();
        }
        if (parenthesized && self.take_p(')'))
            || (!parenthesized && matches!(self.token(), Token::EndLine | Token::Op("->")))
        {
            if parenthesized {
                self.groups -= 1;
            }
            return Ok(params);
        }
        loop {
            let mut kind = match self.token() {
                Token::Op("*") => {
                    self.bump();
                    ParamKind::Rest
                }
                Token::Op("**") => {
                    self.bump();
                    ParamKind::KeywordRest
                }
                _ => ParamKind::Positional,
            };
            let name = self.name()?;
            let mut ty = None;
            let default = if self.take_p(':') {
                if parenthesized {
                    self.line_breaks();
                }
                if kind == ParamKind::Positional
                    && matches!(
                        self.token(),
                        Token::P(',' | ')') | Token::EndLine | Token::Op("->")
                    )
                {
                    kind = ParamKind::Keyword;
                    None
                } else if kind == ParamKind::Positional && self.keyword_default(parenthesized) {
                    kind = ParamKind::Keyword;
                    Some(if parenthesized {
                        self.expr(0)?
                    } else {
                        self.line_expr(0)?
                    })
                } else {
                    let annotation = self.type_expr(1, false)?;
                    if matches!(kind, ParamKind::Rest | ParamKind::KeywordRest)
                        && !annotation.captures(kind == ParamKind::KeywordRest)
                    {
                        return self.err("capture annotation must accept its collection type");
                    }
                    ty = Some(annotation);
                    if kind == ParamKind::Positional && self.take_p(':') {
                        kind = ParamKind::Keyword;
                        if !matches!(
                            self.token(),
                            Token::P(',' | ')') | Token::EndLine | Token::Op("->")
                        ) {
                            return self
                                .err("typed required keyword must end after trailing colon");
                        }
                    }
                    if self.token() == &Token::Op("=") {
                        if kind != ParamKind::Positional {
                            return self.err("capture parameters cannot have defaults");
                        }
                        self.bump();
                        if parenthesized {
                            self.line_breaks();
                        }
                        Some(if parenthesized {
                            self.expr(0)?
                        } else {
                            self.line_expr(0)?
                        })
                    } else {
                        None
                    }
                }
            } else if self.token() == &Token::Op("=") {
                self.bump();
                if kind != ParamKind::Positional {
                    return self.err("capture parameters cannot have defaults");
                }
                if parenthesized {
                    self.lines();
                }
                Some(if parenthesized {
                    self.expr(0)?
                } else {
                    self.line_expr(0)?
                })
            } else {
                None
            };
            match kind {
                ParamKind::Positional if rest || keywords || keyword_rest => {
                    return self
                        .err("positional parameters must precede rest and keyword parameters");
                }
                ParamKind::Rest => {
                    if rest || keywords || keyword_rest {
                        return self.err("invalid rest parameter order");
                    }
                    rest = true;
                }
                ParamKind::Keyword => {
                    if keyword_rest {
                        return self.err("keyword parameter follows keyword rest");
                    }
                    keywords = true;
                }
                ParamKind::KeywordRest => {
                    if keyword_rest {
                        return self.err("duplicate keyword rest parameter");
                    }
                    keyword_rest = true;
                }
                _ => (),
            }
            self.locals.insert(name.clone());
            self.declared_it |= name == "it";
            params.push(Parameter {
                name,
                kind,
                default,
                ty,
            });
            if parenthesized {
                self.lines();
                if self.take_p(')') {
                    self.groups -= 1;
                    break;
                }
            } else if matches!(self.token(), Token::EndLine | Token::Eof | Token::Op("->")) {
                break;
            }
            self.expect_p(',')?;
            if parenthesized {
                self.lines();
            }
        }
        Ok(params)
    }
    fn token(&self) -> &Token {
        &self.tokens[self.pos].token
    }
    fn err<T>(&self, message: &str) -> Result<T> {
        Err(Error::syntax(self.tokens[self.pos].offset, message))
    }
    fn bump(&mut self) -> Token {
        let t = self.token().clone();
        if !matches!(t, Token::Eof) {
            self.pos += 1;
        }
        t
    }
    fn word(&mut self, w: &str) -> bool {
        if matches!(self.token(),Token::Word(s) if s==w) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_word(&mut self, w: &str) -> Result<()> {
        if self.word(w) {
            Ok(())
        } else {
            self.err(&format!("expected {w}"))
        }
    }
    fn take_p(&mut self, c: char) -> bool {
        if self.token() == &Token::P(c) {
            self.pos += 1;
            true
        } else {
            false
        }
    }
    fn expect_p(&mut self, c: char) -> Result<()> {
        if self.take_p(c) {
            Ok(())
        } else {
            self.err(&format!("expected {c}"))
        }
    }
    fn lines(&mut self) {
        while matches!(self.token(), Token::EndLine) {
            self.pos += 1;
        }
    }
    fn line_breaks(&mut self) {
        while self.token() == &Token::EndLine
            && self.tokens[self.pos].line != self.tokens[self.pos].end_line
        {
            self.pos += 1;
        }
    }
    fn name(&mut self) -> Result<String> {
        if let Token::Word(w) = self.bump() {
            if reserved(&w) {
                return self.err("reserved name");
            }
            Ok(w)
        } else {
            self.err("expected name")
        }
    }
    fn enum_name(&mut self) -> Result<String> {
        match self.bump() {
            Token::Word(name) if !keyword(&name) => Ok(name),
            _ => self.err("expected enum identifier"),
        }
    }
    fn at_end(&self) -> bool {
        matches!(self.token(), Token::Eof)
            || matches!(self.token(),Token::Word(s) if matches!(s.as_str(),"end"|"else"|"elsif"|"when"))
    }
    fn enter(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.err("syntax nesting too deep")
        } else {
            Ok(())
        }
    }
    fn declare_target(&mut self, target: &Target) {
        match target {
            Target::Typed(target, _) => self.declare_target(target),
            Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) => {
                self.locals.insert(name.clone());
                self.declared_it |= name == "it";
            }
            Target::Tuple(parts) => {
                for (part, _) in parts {
                    if let Some(part) = part {
                        self.declare_target(part);
                    }
                }
            }
            _ => (),
        }
    }
    fn block(&mut self, stop: &[&str]) -> Result<Vec<Stmt>> {
        self.enter()?;
        let mut body = Vec::new();
        self.lines();
        while !matches!(self.token(),Token::Word(w) if stop.contains(&w.as_str()))
            && !(self.token() == &Token::P('}') && stop.contains(&"}"))
        {
            if matches!(self.token(), Token::Eof) {
                return self.err("unexpected end of source");
            }
            body.push(self.statement()?);
            self.lines();
        }
        self.depth -= 1;
        Ok(body)
    }
    fn statement(&mut self) -> Result<Stmt> {
        let stmt = self.plain_statement()?;
        let modifier = match self.token() {
            Token::Word(w) if matches!(w.as_str(), "if" | "unless" | "while" | "until") => {
                w.clone()
            }
            _ => return Ok(stmt),
        };
        if !matches!(
            stmt,
            Stmt::Expr(_) | Stmt::Assign(..) | Stmt::Return(_) | Stmt::Break(_) | Stmt::Next(_)
        ) {
            return self
                .err("modifier requires an expression, assignment, or leaf control statement");
        }
        self.bump();
        let mut condition = self.line_expr(0)?;
        if matches!(modifier.as_str(), "unless" | "until") {
            condition = self.negate(condition)?;
        }
        Ok(if matches!(modifier.as_str(), "while" | "until") {
            Stmt::While(condition, vec![stmt])
        } else {
            Stmt::If(condition, vec![stmt], Vec::new())
        })
    }
    fn plain_statement(&mut self) -> Result<Stmt> {
        if matches!(self.token(), Token::Word(w) if w == "module")
            && matches!(&self.tokens[self.pos + 1].token, Token::Word(w) if !keyword(w))
            && self.tokens[self.pos].line == self.tokens[self.pos + 1].line
        {
            return self.err("source module declarations are not implemented");
        }
        if self.word("if") {
            return self.if_stmt(false);
        }
        if self.word("unless") {
            return self.if_stmt(true);
        }
        if self.word("while") {
            return self.while_stmt(false);
        }
        if self.word("until") {
            return self.while_stmt(true);
        }
        if self.word("for") {
            return self.for_stmt();
        }
        for flow in ["return", "break", "next"] {
            if self.word(flow) {
                let modifier = matches!(self.token(), Token::Word(w) if matches!(w.as_str(), "if" | "unless" | "while" | "until"));
                let value = if !modifier && self.starts_expression() {
                    let first = self.line_expr(0)?;
                    Some(if flow == "return" {
                        self.return_values(first)?
                    } else {
                        first
                    })
                } else {
                    None
                };
                return Ok(match flow {
                    "return" => Stmt::Return(value),
                    "break" => Stmt::Break(value),
                    _ => Stmt::Next(value),
                });
            }
        }
        if self.assignment_ahead() {
            let target = self.target(true, false)?;
            self.lines();
            let Token::Op(op) = self.bump() else {
                return self.err("expected assignment operator");
            };
            if !assignment(op) {
                return self.err("expected assignment operator");
            }
            if op != "=" && matches!(target, Target::Tuple(_)) {
                return self.err("compound destructuring assignment is invalid");
            }
            self.lines();
            let first = self.line_expr(0)?;
            let rhs = if matches!(target, Target::Tuple(_)) && self.take_p(',') {
                let mut items = vec![first];
                loop {
                    if self.at_end() || self.token() == &Token::EndLine {
                        break;
                    }
                    items.push(self.line_expr(0)?);
                    if !self.take_p(',') {
                        break;
                    }
                }
                let depth = 1 + items.iter().map(|e| e.depth).max().unwrap_or(0);
                self.make(Node::Array(items), depth)?
            } else {
                first
            };
            self.declare_target(&target);
            return Ok(Stmt::Assign(target, op, rhs));
        }
        Ok(Stmt::Expr(self.line_expr(0)?))
    }
    fn return_values(&mut self, first: Expr) -> Result<Expr> {
        if self.token() != &Token::P(',') || self.tokens[self.pos].line != self.previous().line {
            return Ok(first);
        }
        let mut items = vec![first];
        while self.token() == &Token::P(',') && self.tokens[self.pos].line == self.previous().line {
            self.bump();
            self.line_breaks();
            items.push(self.line_expr(0)?);
        }
        let depth = 1 + items.iter().map(|e| e.depth).max().unwrap_or(0);
        self.make(Node::Array(items), depth)
    }
    fn negate(&self, expr: Expr) -> Result<Expr> {
        let depth = expr.depth + 1;
        self.make(Node::Unary("!", Box::new(expr)), depth)
    }
    fn while_stmt(&mut self, until: bool) -> Result<Stmt> {
        let previous = self.loop_condition.replace(self.groups);
        let mut cond = self.line_expr(0)?;
        self.loop_condition = previous;
        if until {
            cond = self.negate(cond)?;
        }
        self.word("do");
        let body = self.block(&["end"])?;
        self.expect_word("end")?;
        Ok(Stmt::While(cond, body))
    }
    fn for_stmt(&mut self) -> Result<Stmt> {
        let target = self.target(false, false)?;
        if !target.is_binding() {
            return self.err("invalid for loop target");
        }
        self.expect_word("in")?;
        let previous = self.loop_condition.replace(self.groups);
        let iterable = self.line_expr(0)?;
        self.loop_condition = previous;
        self.word("do");
        self.declare_target(&target);
        let body = self.block(&["end"])?;
        self.expect_word("end")?;
        Ok(Stmt::For(target, iterable, body))
    }
    fn target(&mut self, first_expression: bool, typed: bool) -> Result<Target> {
        self.enter()?;
        let mut parts = Vec::new();
        let mut tuple = false;
        let mut has_rest = false;
        loop {
            let rest = self.token() == &Token::Op("*");
            if rest {
                self.bump();
                if has_rest {
                    return self.err("duplicate rest target");
                }
                has_rest = true;
                tuple = true;
            }
            let mut value = if rest
                && (matches!(self.token(), Token::P(',' | ')' | ']') | Token::Op("="))
                    || matches!(self.token(), Token::Word(w) if w=="in"))
            {
                None
            } else {
                let grouped = !first_expression || !parts.is_empty() || rest;
                let close = if grouped && self.take_p('(') {
                    Some(')')
                } else if grouped && self.take_p('[') {
                    Some(']')
                } else {
                    None
                };
                if let Some(close) = close {
                    self.lines();
                    let inner = self.target(false, typed)?;
                    self.lines();
                    self.expect_p(close)?;
                    Some(match inner {
                        Target::Tuple(_) => inner,
                        _ => Target::Tuple(vec![(Some(inner), false)]),
                    })
                } else {
                    Some(if typed && matches!(self.token(), Token::Word(_)) {
                        let name = self.name()?;
                        Target::Value(self.make(Node::Var(name), 1)?)
                    } else {
                        Target::Value(self.line_expr(0)?)
                    })
                }
            };
            if typed && value.is_some() && self.take_p(':') {
                let ty = self.type_expr(1, false)?;
                if rest && !ty.captures(false) {
                    return self.err("rest target annotation must accept an array");
                }
                value = Some(Target::Typed(Box::new(value.take().unwrap()), ty));
            }
            parts.push((value, rest));
            if !self.take_p(',') {
                break;
            }
            tuple = true;
            self.lines();
        }
        self.depth -= 1;
        let target = if tuple {
            Target::Tuple(parts)
        } else {
            parts.pop().unwrap().0.unwrap()
        };
        if target.depth() > MAX_DEPTH {
            return self.err("assignment nesting too deep");
        }
        Ok(target)
    }
    fn assignment_ahead(&self) -> bool {
        let mut nesting = 0usize;
        let mut comma = false;
        for (i, lexeme) in self.tokens.from(self.pos).enumerate() {
            match &lexeme.token {
                Token::P('(' | '[' | '{') => nesting += 1,
                Token::P(')' | ']' | '}') => {
                    if nesting == 0 {
                        return false;
                    }
                    nesting -= 1;
                }
                Token::Op("=")
                    if i >= 3
                        && self.tokens[self.pos + i - 3].token == Token::P(':')
                        && self.symbol_start(self.pos + i - 3)
                        && self.tokens[self.pos + i - 2].token == Token::P('[')
                        && self.tokens[self.pos + i - 1].token == Token::P(']')
                        && self.tokens[self.pos + i - 1].end == lexeme.offset => {}
                Token::Op(op) if nesting == 0 && assignment(op) => return true,
                Token::EndLine if nesting == 0 => {
                    if lexeme.line == lexeme.end_line {
                        return false;
                    }
                    let next = self
                        .tokens
                        .from(self.pos + i + 1)
                        .find(|l| !matches!(l.token, Token::EndLine));
                    if !comma
                        && !next.is_some_and(|l| {
                            l.token == Token::P('.')
                                || matches!(l.token, Token::Op(op) if assignment(op))
                        })
                    {
                        return false;
                    }
                }
                Token::Word(w)
                    if nesting == 0
                        && reserved(w)
                        && (i == 0 || self.tokens[self.pos + i - 1].token != Token::P('.')) =>
                {
                    return false;
                }
                Token::Eof => return false,
                _ => (),
            }
            if !matches!(lexeme.token, Token::EndLine) {
                comma = lexeme.token == Token::P(',');
            }
        }
        false
    }
    fn if_stmt(&mut self, unless: bool) -> Result<Stmt> {
        self.enter()?;
        let mut cond = self.line_expr(0)?;
        if unless {
            cond = self.negate(cond)?;
        }
        self.word("then");
        self.lines();
        let yes = self.block(&["else", "elsif", "end"])?;
        let no = if self.word("elsif") {
            if unless {
                return self.err("unless does not support elsif");
            }
            vec![self.if_stmt(false)?]
        } else if self.word("else") {
            self.lines();
            let no = self.block(&["end"])?;
            self.expect_word("end")?;
            no
        } else {
            self.expect_word("end")?;
            Vec::new()
        };
        self.depth -= 1;
        Ok(Stmt::If(cond, yes, no))
    }
    fn if_expr(&mut self, unless: bool) -> Result<Expr> {
        self.enter()?;
        let mut cond = self.line_expr(0)?;
        if unless {
            cond = self.negate(cond)?;
        }
        self.word("then");
        self.lines();
        let yes = self.expr(0)?;
        self.lines();
        let no = if self.word("elsif") {
            if unless {
                return self.err("unless does not support elsif");
            }
            self.if_expr(false)?
        } else if self.word("else") {
            self.lines();
            let no = self.expr(0)?;
            self.lines();
            self.expect_word("end")?;
            no
        } else {
            self.expect_word("end")?;
            self.make(Node::Literal(Value::nil()), 1)?
        };
        let depth = 1 + cond.depth.max(yes.depth).max(no.depth);
        self.depth -= 1;
        self.make(
            Node::Conditional(Box::new(cond), Box::new(yes), Box::new(no)),
            depth,
        )
    }
    fn case_expr(&mut self) -> Result<Expr> {
        self.lines();
        let target = if matches!(self.token(), Token::Word(w) if w=="when") {
            None
        } else {
            Some(Box::new(self.line_expr(0)?))
        };
        self.lines();
        let mut clauses = Vec::new();
        while self.word("when") {
            let mut values = Vec::new();
            loop {
                let splat = self.token() == &Token::Op("*");
                if splat {
                    self.bump();
                }
                values.push((self.line_expr(0)?, splat));
                if !self.take_p(',') {
                    break;
                }
                self.lines();
            }
            self.word("then");
            self.lines();
            let result = self.expr(0)?;
            clauses.push(When { values, result });
            self.lines();
        }
        if clauses.is_empty() {
            return self.err("case requires a when clause");
        }
        let alternate = if self.word("else") {
            self.lines();
            Some(Box::new(self.expr(0)?))
        } else {
            None
        };
        self.lines();
        self.expect_word("end")?;
        let depth = 1 + target
            .as_ref()
            .map_or(0, |e| e.depth)
            .max(alternate.as_ref().map_or(0, |e| e.depth))
            .max(
                clauses
                    .iter()
                    .map(|c| {
                        c.result
                            .depth
                            .max(c.values.iter().map(|(v, _)| v.depth).max().unwrap_or(0))
                    })
                    .max()
                    .unwrap_or(0),
            );
        self.make(Node::Case(target, clauses, alternate), depth)
    }
    fn make(&self, node: Node, depth: usize) -> Result<Expr> {
        if depth > MAX_DEPTH {
            self.err("expression nesting too deep")
        } else {
            Ok(Expr { node, depth })
        }
    }
    fn expr(&mut self, min: u8) -> Result<Expr> {
        self.enter()?;
        let lhs = self.prefix()?;
        let result = self.expr_tail(lhs, min);
        self.depth -= 1;
        result
    }
    fn line_expr(&mut self, min: u8) -> Result<Expr> {
        self.line_exprs += 1;
        let result = self.expr(min);
        self.line_exprs -= 1;
        result
    }
    // Keep the prefix and tail frames separate so debug builds reach the nesting guard.
    fn prefix(&mut self) -> Result<Expr> {
        match self.bump() {
            Token::Int(n) => self.make(Node::Integer(n), 1),
            Token::BigInt(text, radix) => self.make(Node::BigInteger(text, radix), 1),
            Token::Float(n) => self.make(Node::Literal(Value::float(n)), 1),
            Token::Bytes(b) => self.make(Node::Literal(Value::bytes(b)), 1),
            Token::Template(parts) => self.template(parts, false),
            Token::Words(words) => self.words(*words),
            Token::Invalid(error) => Err(Error::syntax(error.0, error.1)),
            Token::Word(w) => match w.as_str() {
                "nil" => self.make(Node::Literal(Value::nil()), 1),
                "true" => self.make(Node::Literal(Value::boolean(true)), 1),
                "false" => self.make(Node::Literal(Value::boolean(false)), 1),
                "if" | "unless" => self.if_expr(w == "unless"),
                "case" => self.case_expr(),
                "yield" => self.yield_expr(),
                "while" | "until" | "for" => {
                    let stmt = if w == "for" {
                        self.for_stmt()?
                    } else {
                        self.while_stmt(w == "until")?
                    };
                    let depth = stmt.depth();
                    self.make(Node::Loop(Box::new(stmt)), depth)
                }
                _ if reserved(&w) => self.err("expected expression"),
                _ => self.make(Node::Var(w), 1),
            },
            Token::P(':') => self.symbol(),
            Token::P('(') => {
                self.groups += 1;
                self.lines();
                let e = self.expr(0)?;
                self.lines();
                self.expect_p(')')?;
                self.groups -= 1;
                Ok(e)
            }
            Token::P('[') => {
                let a = self.arguments(']')?;
                let d = 1 + a.iter().map(|e| e.depth).max().unwrap_or(0);
                self.make(Node::Array(a), d)
            }
            Token::P('{') => self.hash_expr(),
            Token::Op(op @ (".." | "...")) => {
                if self.groups > 0 {
                    self.lines();
                }
                let end = self.expr(8)?;
                let depth = end.depth + 1;
                self.make(Node::Range(None, Some(Box::new(end)), op == "..."), depth)
            }
            Token::Op(op @ ("-" | "+" | "!")) => self.unary_prefix(op),
            _ => self.err("expected expression"),
        }
    }
    fn unary_prefix(&mut self, op: &'static str) -> Result<Expr> {
        let value = if self.negative_literal(op) {
            self.prefix()?
        } else {
            self.line_breaks();
            self.expr(13)?
        };
        let depth = value.depth + 1;
        self.make(Node::Unary(op, Box::new(value)), depth)
    }
    fn negative_literal(&self, op: &str) -> bool {
        // An adjacent minus belongs to the numeric receiver; power keeps the
        // outer sign. Keep lookahead off the recursive prefix stack frame.
        op == "-"
            && self.tokens[self.pos - 1].end == self.tokens[self.pos].offset
            && matches!(
                self.token(),
                Token::Int(_) | Token::BigInt(..) | Token::Float(_)
            )
            && !self
                .tokens
                .from(self.pos + 1)
                .find(|next| next.token != Token::EndLine || next.line == next.end_line)
                .is_some_and(|next| next.token == Token::Op("**"))
    }
    fn hash_group(&mut self) -> Result<Expr> {
        self.groups += 1;
        let mut entries = Vec::new();
        self.line_breaks();
        if !self.take_p('}') {
            loop {
                let (key, label) = match self.bump() {
                    Token::Word(w) => (w.as_bytes().to_vec(), Some(w)),
                    Token::Bytes(b) => (b, None),
                    _ => return self.err("expected hash label"),
                };
                self.line_breaks();
                self.expect_p(':')?;
                self.line_breaks();
                let value = if matches!(self.token(), Token::P(',' | '}') | Token::Eof) {
                    let Some(name) = label else {
                        return self.err("missing value for hash key");
                    };
                    self.make(Node::Var(name), 1)?
                } else {
                    self.expr(0)?
                };
                entries.push((key, value));
                self.line_breaks();
                if self.take_p('}') {
                    break;
                }
                self.expect_p(',')?;
                self.line_breaks();
                if self.take_p('}') {
                    break;
                }
            }
        }
        let d = 1 + entries.iter().map(|(_, e)| e.depth).max().unwrap_or(0);
        self.groups -= 1;
        self.make(Node::Hash(entries), d)
    }

    fn words(&mut self, words: lexer::Words) -> Result<Expr> {
        let mut values = Vec::with_capacity(words.entries.len());
        for word in words.entries {
            values.push(self.template(word, words.symbol)?);
        }
        let depth = 1 + values.iter().map(|v| v.depth).max().unwrap_or(0);
        self.make(Node::Array(values), depth)
    }

    fn template(&mut self, parts: Vec<Part>, symbol: bool) -> Result<Expr> {
        if !parts.iter().any(|part| matches!(part, Part::Expr(_))) {
            let bytes = lexer::plain(parts);
            let value = if symbol {
                Value::symbol(bytes)
            } else {
                Value::bytes(bytes)
            };
            return self.make(Node::Literal(value), 1);
        }
        let mut values = Vec::with_capacity(parts.len());
        for part in parts {
            values.push(match part {
                Part::Text(bytes) => self.make(Node::Literal(Value::bytes(bytes)), 1)?,
                Part::Expr(tokens) => self.interpolation(tokens)?,
            });
        }
        let depth = 1 + values.iter().map(|v| v.depth).max().unwrap_or(0);
        self.make(Node::Template(values, symbol), depth)
    }

    fn interpolation(&mut self, mut tokens: Vec<Lexeme>) -> Result<Expr> {
        while tokens.len() >= 2 {
            let tail = &tokens[tokens.len() - 2];
            if tail.token != Token::EndLine || tail.line == tail.end_line {
                break;
            }
            tokens.remove(tokens.len() - 2);
        }
        let mut parser = Parser {
            source: self.source,
            lex_depth: self.lex_depth + 1,
            tokens: Tokens::new(tokens),
            pos: 0,
            depth: self.depth,
            groups: 0,
            line_exprs: 0,
            command_depth: 0,
            ternaries: Vec::new(),
            command_group: 0,
            loop_condition: None,
            locals: std::mem::take(&mut self.locals),
            declared_it: self.declared_it,
            type_structural_error: false,
        };
        while parser.token() == &Token::EndLine
            && parser.tokens[parser.pos].line != parser.tokens[parser.pos].end_line
        {
            parser.pos += 1;
        }
        let result = (|| {
            let expr = parser.line_expr(0)?;
            if parser.token() != &Token::Eof {
                return parser.err("string interpolation must contain a single expression");
            }
            Ok(expr)
        })();
        self.locals = parser.locals;
        self.declared_it = parser.declared_it;
        result
    }

    fn expand_modulo(&mut self) -> Result<()> {
        // A quoted index can extend past a tentative percent-literal delimiter.
        // Re-lex its suffix through the next intact token boundary.
        let limit = self.tokens.last().unwrap().offset;
        let mut tokens = lexer::modulo(self.source, &self.tokens[self.pos], limit, self.lex_depth)?;
        let mut cursor = tokens.pop().unwrap();
        let mut finish = self.pos + 1;
        loop {
            while self.tokens[finish].offset < cursor.offset {
                finish += 1;
            }
            let end = self.tokens[finish - 1].end;
            if end <= cursor.offset {
                break;
            }
            let mut suffix = lexer::resume(
                self.source,
                &cursor,
                end,
                limit,
                self.lex_depth,
                tokens.last(),
            )?;
            cursor = suffix.pop().unwrap();
            tokens.extend(suffix);
        }
        self.tokens.replace(self.pos..finish, tokens);
        Ok(())
    }

    fn symbol(&mut self) -> Result<Expr> {
        if !self.symbol_start(self.pos - 1) {
            return self.err("expected symbol");
        }
        let bytes = match self.bump() {
            Token::Word(w) => w.into_bytes(),
            Token::Bytes(b) => b,
            Token::Op(op) => op.as_bytes().to_vec(),
            Token::P('[') => {
                self.expect_p(']')?;
                if self.token() == &Token::Op("=")
                    && self.previous().end == self.tokens[self.pos].offset
                {
                    self.bump();
                    b"[]=".to_vec()
                } else {
                    b"[]".to_vec()
                }
            }
            _ => return self.err("expected symbol"),
        };
        self.make(Node::Literal(Value::symbol(bytes)), 1)
    }
    fn expr_tail(&mut self, mut lhs: Expr, min: u8) -> Result<Expr> {
        loop {
            if let Some(next) = self.continuation_position(min) {
                self.pos = next;
            }
            if self.command_start(&lhs, min) {
                self.command_depth += 1;
                if self.command_depth > 64 {
                    return self.err("parenless call nesting too deep");
                }
                let group = std::mem::replace(&mut self.command_group, self.groups);
                let args = self.command_arguments()?;
                self.command_group = group;
                self.command_depth -= 1;
                let depth = 1 + lhs
                    .depth
                    .max(args.iter().map(|a| a.value.depth).max().unwrap_or(0));
                let node = match lhs.node {
                    Node::Var(name) => Node::Call(name, args),
                    Node::Member(receiver, name) => Node::Method(receiver, name, args),
                    _ => unreachable!(),
                };
                lhs = self.make(node, depth)?;
                continue;
            }
            let brace = self.token() == &Token::P('{');
            let do_block =
                matches!(self.token(), Token::Word(w) if w == "do") && self.can_attach_do();
            if (brace || do_block)
                && (do_block || self.tokens[self.pos].line == self.previous().end_line)
            {
                let block = self.attached_block(brace)?;
                if let Node::BlockCall(call, _) = lhs.node {
                    lhs = *call;
                }
                let depth = 1 + lhs
                    .depth
                    .max(block.body.iter().map(Stmt::depth).max().unwrap_or(0));
                lhs = self.make(Node::BlockCall(Box::new(lhs), block), depth)?;
                continue;
            }
            if self.take_p('(') {
                let Node::Var(name) = lhs.node else {
                    return self.err("only named functions are callable");
                };
                let args = self.call_arguments()?;
                let d = 1 + args.iter().map(|a| a.value.depth).max().unwrap_or(0);
                lhs = self.make(Node::Call(name, args), d)?;
                continue;
            }
            if self.token() == &Token::Op("::") {
                self.bump();
                self.line_breaks();
                let Token::Word(name) = self.bump() else {
                    return self.err("expected scoped member name");
                };
                let args = if self.take_p('(') {
                    Some(self.call_arguments()?)
                } else {
                    None
                };
                let depth = 1 + lhs.depth.max(args.as_ref().map_or(0, |args| {
                    args.iter().map(|arg| arg.value.depth).max().unwrap_or(0)
                }));
                lhs = self.make(Node::Scope(Box::new(lhs), name, args), depth)?;
                continue;
            }
            if self.take_p('.') {
                let name = match self.bump() {
                    Token::Word(name) => name,
                    Token::Op("<=>") => "<=>".to_owned(),
                    _ => return self.err("expected member name"),
                };
                lhs = if self.take_p('(') {
                    let args = self.call_arguments()?;
                    let depth = 1 + lhs
                        .depth
                        .max(args.iter().map(|a| a.value.depth).max().unwrap_or(0));
                    self.make(Node::Method(Box::new(lhs), name, args), depth)?
                } else {
                    let depth = lhs.depth + 1;
                    self.make(Node::Member(Box::new(lhs), name), depth)?
                };
                continue;
            }
            if self.take_p('[') {
                let indexes = self.arguments(']')?;
                if indexes.is_empty() {
                    return self.err("expected index");
                }
                let d = 1 + lhs
                    .depth
                    .max(indexes.iter().map(|e| e.depth).max().unwrap_or(0));
                lhs = self.make(Node::Index(Box::new(lhs), indexes), d)?;
                continue;
            }
            if min <= 2 && self.take_p('?') {
                self.lines();
                self.ternaries.push(self.groups);
                let yes = self.expr(0)?;
                self.ternaries.pop();
                self.expect_p(':')?;
                self.lines();
                let no = self.expr(2)?;
                let depth = 1 + lhs.depth.max(yes.depth).max(no.depth);
                lhs = self.make(
                    Node::Conditional(Box::new(lhs), Box::new(yes), Box::new(no)),
                    depth,
                )?;
                continue;
            }
            if matches!(self.token(), Token::Words(words) if words.ambiguous) {
                self.expand_modulo()?;
            }
            let Token::Op(op) = self.token() else {
                break;
            };
            let op = *op;
            let Some((left, right)) = binding_power(op) else {
                break;
            };
            if left < min {
                break;
            }
            self.bump();
            if matches!(op, ".." | "...") {
                if self.groups > 0 {
                    self.lines();
                }
                let end = if self.starts_expression() {
                    Some(Box::new(self.expr(right)?))
                } else {
                    None
                };
                let depth = 1 + lhs.depth.max(end.as_ref().map_or(0, |e| e.depth));
                lhs = self.make(Node::Range(Some(Box::new(lhs)), end, op == "..."), depth)?;
                continue;
            }
            self.line_breaks();
            let rhs = self.expr(right)?;
            let depth = 1 + lhs.depth.max(rhs.depth);
            lhs = self.make(Node::Binary(op, Box::new(lhs), Box::new(rhs)), depth)?;
        }
        Ok(lhs)
    }
    fn previous(&self) -> &Lexeme {
        self.tokens
            .range(0..self.pos)
            .rev()
            .find(|t| t.token != Token::EndLine)
            .unwrap()
    }
    fn attached_block(&mut self, brace: bool) -> Result<Block> {
        self.bump();
        self.lines();
        let outer = self.locals.clone();
        let outer_it = self.declared_it;
        let infer_it = !outer_it;
        let mut params = Vec::new();
        let explicit = if self.token() == &Token::Op("||") {
            self.bump();
            true
        } else if self.take_p('|') {
            self.lines();
            if !self.take_p('|') {
                loop {
                    let target = if self.take_p('(') {
                        let target = self.target(false, true)?;
                        self.lines();
                        self.expect_p(')')?;
                        match target {
                            Target::Tuple(_) => target,
                            _ => Target::Tuple(vec![(Some(target), false)]),
                        }
                    } else if self.take_p('[') {
                        let target = self.target(false, true)?;
                        self.lines();
                        self.expect_p(']')?;
                        match target {
                            Target::Tuple(_) => target,
                            _ => Target::Tuple(vec![(Some(target), false)]),
                        }
                    } else {
                        let name = self.name()?;
                        let target = Target::Value(self.make(Node::Var(name), 1)?);
                        if self.take_p(':') {
                            Target::Typed(Box::new(target), self.type_expr(0, true)?)
                        } else {
                            target
                        }
                    };
                    if !target.is_binding() {
                        return self.err("invalid block parameter");
                    }
                    self.declare_target(&target);
                    params.push(target);
                    self.lines();
                    if self.take_p('|') {
                        break;
                    }
                    self.expect_p(',')?;
                    self.lines();
                }
            }
            true
        } else {
            false
        };
        if !explicit {
            self.locals.insert("it".into());
            for n in 1..=9 {
                self.locals.insert(format!("_{n}"));
            }
        }
        let previous_loop = self.loop_condition.take();
        let command_depth = std::mem::replace(&mut self.command_depth, 0);
        let body = self.block(if brace { &["}"] } else { &["end"] })?;
        self.command_depth = command_depth;
        self.loop_condition = previous_loop;
        if brace {
            self.expect_p('}')?;
        } else {
            self.expect_word("end")?;
        }
        self.locals = outer;
        self.declared_it = outer_it;
        Ok(Block {
            params,
            body,
            implicit: !explicit,
            infer_it,
        })
    }
    fn can_attach_do(&self) -> bool {
        (self.command_depth == 0 || self.groups > self.command_group)
            && self.loop_condition.is_none_or(|group| self.groups > group)
    }
    fn yield_expr(&mut self) -> Result<Expr> {
        let line = self.previous().line;
        let mut next = self.pos;
        while self.tokens[next].token == Token::EndLine
            && self.tokens[next].line != self.tokens[next].end_line
        {
            next += 1;
        }
        if self.tokens[next].token == Token::P('(') {
            self.pos = next;
        }
        let args = if self.take_p('(') {
            self.arguments(')')?
        } else {
            let mut args = Vec::new();
            if self.tokens[self.pos].line == line && self.starts_expression() {
                args.push(self.line_expr(0)?);
                while self.token() == &Token::P(',')
                    && self.tokens[self.pos].line == line
                    && self.tokens[self.pos + 1].line == line
                {
                    self.bump();
                    args.push(self.line_expr(0)?);
                }
            }
            args
        };
        let depth = 1 + args.iter().map(|arg| arg.depth).max().unwrap_or(0);
        self.make(Node::Yield(args), depth)
    }
    fn continuation_position(&self, min: u8) -> Option<usize> {
        if self.token() != &Token::EndLine {
            return None;
        }
        let mut next = self.pos;
        while self.tokens[next].token == Token::EndLine {
            if self.tokens[next].line == self.tokens[next].end_line {
                return None;
            }
            next += 1;
        }
        let lexeme = &self.tokens[next];
        let continues = match lexeme.token {
            Token::Word(ref word) if word == "do" => self.can_attach_do(),
            Token::P('.') | Token::Op("::") => true,
            Token::P('?') => min <= 2,
            Token::P('(' | '[') => self.line_exprs == 0 && self.groups > 0,
            Token::Op(op) => {
                let (left, _) = binding_power(op)?;
                if left < min {
                    return None;
                }
                if self.line_exprs == 0 {
                    if op == "/" {
                        return None;
                    }
                    return (self.groups > 0).then_some(next);
                }
                match op {
                    "+" | "-" => self
                        .tokens
                        .from(next + 1)
                        .find(|t| t.token != Token::EndLine || t.line == t.end_line)
                        .is_some_and(|operand| {
                            !matches!(operand.token, Token::Eof | Token::EndLine)
                                && (operand.line > lexeme.end_line || operand.offset > lexeme.end)
                        }),
                    "*" => !self.splat_assignment_ahead(next),
                    "/" => false,
                    _ => true,
                }
            }
            _ => false,
        };
        continues.then_some(next)
    }
    fn splat_assignment_ahead(&self, start: usize) -> bool {
        let mut groups = 0usize;
        let mut previous = &self.tokens[start];
        let operand = &self.tokens[start + 1];
        let shaped = matches!(operand.token, Token::P(',') | Token::Op("="))
            || operand.offset == previous.end;
        let mut comma = false;
        for token in self.tokens.from(start + 1) {
            if token.line > self.tokens[start].line + 64 {
                return false;
            }
            if token.token == Token::EndLine {
                if token.line == token.end_line {
                    return false;
                }
                continue;
            }
            if token.line > previous.end_line
                && groups == 0
                && previous.token != Token::P(',')
                && !((shaped || comma)
                    && (token.token == Token::Op("=")
                        || (token.token == Token::P('.')
                            && matches!(previous.token, Token::Word(_) | Token::P(')' | ']')))))
            {
                return false;
            }
            if groups == 0 && token.token != Token::Op("=") {
                let allowed = if previous.token == Token::P('.') {
                    matches!(token.token, Token::Word(_))
                } else {
                    match &token.token {
                        Token::Word(w) => !reserved(w),
                        Token::P(',' | '.' | '(' | ')' | '[' | ']') | Token::Op("*") => true,
                        _ => false,
                    }
                };
                if !allowed {
                    return false;
                }
            }
            match token.token {
                Token::P('(' | '[') => groups += 1,
                Token::P(')' | ']') => {
                    let Some(next) = groups.checked_sub(1) else {
                        return false;
                    };
                    groups = next;
                }
                Token::Op("=") if groups == 0 => return true,
                Token::P(',') if groups == 0 => comma = true,
                Token::Eof => return false,
                _ => (),
            }
            previous = token;
        }
        false
    }
    fn keyword_label(&self, pos: usize) -> bool {
        matches!(self.tokens[pos].token, Token::Word(_))
            && self
                .tokens
                .get(pos + 1)
                .is_some_and(|t| t.token == Token::P(':'))
    }
    fn command_start(&self, lhs: &Expr, min: u8) -> bool {
        if self.line_exprs == 0 || min > 14 || !matches!(lhs.node, Node::Var(_) | Node::Member(..))
        {
            return false;
        }
        let local = match &lhs.node {
            Node::Var(name) if name == "self" => return false,
            Node::Var(name) => self.locals.contains(name),
            _ => false,
        };
        let previous = self.previous();
        let next = &self.tokens[self.pos];
        if next.line != previous.end_line {
            return false;
        }
        if self.keyword_label(self.pos) {
            return true;
        }
        match next.token {
            Token::P(':')
                if self.ternaries.last() == Some(&self.groups)
                    && matches!(self.tokens[self.pos + 1].token, Token::Bytes(_)) =>
            {
                false
            }
            Token::P('[') => !local && previous.end != next.offset,
            Token::Words(..) => !local && previous.end != next.offset,
            Token::Op(op @ ("*" | "**" | "/" | "&")) => {
                // Go v0.70.0 locates a power token at its second star.
                let start = next.offset + usize::from(op == "**");
                !local
                    && previous.end != start
                    && self.tokens.get(self.pos + 1).is_some_and(|t| {
                        !matches!(t.token, Token::EndLine | Token::Eof)
                            && t.line == next.end_line
                            && t.offset == next.end
                    })
            }
            _ => self.command_argument_start(self.pos, false),
        }
    }
    fn command_argument_start(&self, pos: usize, after_comma: bool) -> bool {
        if self.keyword_label(pos) {
            return true;
        }
        match &self.tokens[pos].token {
            Token::Word(w) => !reserved(w) || matches!(w.as_str(), "case" | "for" | "yield"),
            Token::Int(_)
            | Token::BigInt(..)
            | Token::Float(_)
            | Token::Bytes(_)
            | Token::Template(_)
            | Token::Words(..) => true,
            Token::P(':') => self.symbol_start(pos),
            Token::Op("!") => true,
            Token::P('[') | Token::Op("*" | "**" | "&") => after_comma,
            _ => false,
        }
    }
    fn symbol_start(&self, pos: usize) -> bool {
        let colon = &self.tokens[pos];
        if pos > 0
            && matches!(self.tokens[pos - 1].token, Token::Word(_))
            && self.tokens[pos - 1].end == colon.offset
        {
            return false;
        }
        self.tokens.get(pos + 1).is_some_and(|t| {
            t.offset == colon.end
                && (matches!(t.token, Token::Word(_) | Token::Bytes(_))
                    || matches!(
                        t.token,
                        Token::Op(
                            "+" | "-"
                                | "*"
                                | "/"
                                | "%"
                                | "**"
                                | "<<"
                                | "&"
                                | "<"
                                | ">"
                                | "<="
                                | "<=>"
                                | ">="
                                | "=="
                                | "==="
                                | "!="
                                | "!"
                                | "&&"
                                | "||"
                        )
                    )
                    || (t.token == Token::P('[')
                        && self
                            .tokens
                            .get(pos + 2)
                            .is_some_and(|end| end.token == Token::P(']') && end.offset == t.end)))
        })
    }
    fn command_arguments(&mut self) -> Result<Vec<Argument>> {
        let mut args = Vec::new();
        let mut keywords = false;
        loop {
            let argument = self.call_argument(false)?;
            let keyword = matches!(
                argument.kind,
                ArgumentKind::Keyword(_) | ArgumentKind::KeywordSplat
            );
            if keywords && !keyword {
                return self.err("positional arguments cannot follow keywords");
            }
            keywords |= keyword;
            args.push(argument);
            let last = self.previous();
            if self.token() != &Token::P(',')
                || self.tokens[self.pos].line != last.line
                || self.tokens[self.pos + 1].line != last.line
                || !self.command_argument_start(self.pos + 1, true)
            {
                break;
            }
            self.bump();
        }
        Ok(args)
    }
    fn arguments(&mut self, close: char) -> Result<Vec<Expr>> {
        self.groups += 1;
        let mut args = Vec::new();
        self.lines();
        if self.take_p(close) {
            self.groups -= 1;
            return Ok(args);
        }
        loop {
            args.push(self.expr(0)?);
            self.lines();
            if self.take_p(close) {
                break;
            }
            self.expect_p(',')?;
            self.lines();
            if self.take_p(close) {
                break;
            }
        }
        self.groups -= 1;
        Ok(args)
    }
    fn call_arguments(&mut self) -> Result<Vec<Argument>> {
        self.groups += 1;
        let mut args = Vec::new();
        let mut keywords = false;
        self.line_breaks();
        if self.take_p(')') {
            self.groups -= 1;
            return Ok(args);
        }
        loop {
            let argument = self.call_argument(true)?;
            let keyword = matches!(
                argument.kind,
                ArgumentKind::Keyword(_) | ArgumentKind::KeywordSplat
            );
            if keywords && !keyword {
                return self.err("positional arguments cannot follow keywords");
            }
            keywords |= keyword;
            args.push(argument);
            self.line_breaks();
            if self.take_p(')') {
                break;
            }
            self.expect_p(',')?;
            self.line_breaks();
            if self.take_p(')') {
                break;
            }
        }
        self.groups -= 1;
        Ok(args)
    }
    fn call_argument(&mut self, parenthesized: bool) -> Result<Argument> {
        let kind = self.argument_kind();
        if parenthesized || matches!(kind, ArgumentKind::Splat | ArgumentKind::KeywordSplat) {
            self.line_breaks();
        }
        let value = match self.literal_argument(&kind, parenthesized)? {
            Some(value) => value,
            None => self.expr(0)?,
        };
        Ok(Argument { kind, value })
    }
    // Keep lookahead temporaries out of the recursive argument frame.
    fn argument_kind(&mut self) -> ArgumentKind {
        if self.token() == &Token::Op("**") {
            self.bump();
            ArgumentKind::KeywordSplat
        } else if self.keyword_label(self.pos) {
            let Token::Word(name) = self.bump() else {
                unreachable!()
            };
            self.bump();
            ArgumentKind::Keyword(name)
        } else if self.token() == &Token::Op("*") {
            self.bump();
            ArgumentKind::Splat
        } else {
            ArgumentKind::Positional
        }
    }
    fn literal_argument(
        &mut self,
        kind: &ArgumentKind,
        parenthesized: bool,
    ) -> Result<Option<Expr>> {
        if let ArgumentKind::Keyword(name) = kind {
            let shorthand = self.token() == &Token::P(',')
                || (parenthesized && self.token() == &Token::P(')'))
                || (!parenthesized
                    && (self.token() == &Token::Eof
                        || (self.token() == &Token::EndLine
                            && self.tokens[self.pos].line != self.tokens[self.pos].end_line)
                        || self.tokens[self.pos].line != self.tokens[self.pos - 1].end_line));
            if shorthand {
                return Ok(Some(self.make(Node::Var(name.clone()), 1)?));
            }
        } else if parenthesized && matches!(kind, ArgumentKind::Positional) {
            return self.argument_type_literal();
        }
        Ok(None)
    }
    fn starts_expression(&self) -> bool {
        match self.token() {
            Token::Word(w) => {
                !reserved(w)
                    || matches!(
                        w.as_str(),
                        "if" | "unless" | "case" | "while" | "until" | "for" | "yield"
                    )
            }
            Token::Int(_)
            | Token::BigInt(..)
            | Token::Float(_)
            | Token::Bytes(_)
            | Token::Template(_)
            | Token::Words(..) => true,
            Token::P('(' | '[' | '{' | ':') | Token::Op("+" | "-" | "!") => true,
            _ => false,
        }
    }
}

fn assignment(op: &str) -> bool {
    matches!(
        op,
        "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "**=" | "||=" | "&&="
    )
}

fn binding_power(op: &str) -> Option<(u8, u8)> {
    Some(match op {
        "||" => (3, 4),
        "&&" => (4, 5),
        "==" | "!=" | "===" => (5, 6),
        "<" | "<=" | ">" | ">=" | "<=>" => (6, 7),
        ".." | "..." => (7, 8),
        "&" => (9, 10),
        "<<" => (10, 11),
        "+" | "-" => (11, 12),
        "*" | "/" | "%" => (12, 13),
        "**" => (14, 14),
        _ => return None,
    })
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
            | "def"
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
fn keyword(w: &str) -> bool {
    reserved(w)
        || matches!(
            w,
            "export"
                | "self"
                | "private"
                | "property"
                | "getter"
                | "setter"
                | "ensure"
                | "true"
                | "false"
                | "nil"
        )
}
pub(crate) fn unsupported(message: &str) -> Error {
    Error::new(crate::ErrorKind::Syntax, message)
}
