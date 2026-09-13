use crate::{Error, Result, Value};

const MAX_DEPTH: usize = 128;
const MAX_SOURCE: usize = 8 << 20;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Word(String),
    Int(i64),
    Float(f64),
    Bytes(Vec<u8>),
    P(char),
    Op(&'static str),
    EndLine,
    Eof,
}
struct Lexeme {
    token: Token,
    offset: usize,
}

fn lex(source: &str) -> Result<Vec<Lexeme>> {
    if source.len() > MAX_SOURCE {
        return Err(Error::syntax(0, "source exceeds 8 MiB"));
    }
    let s = source.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < s.len() {
        let start = i;
        let token = match s[i] {
            b' ' | b'\t' | b'\r' => {
                i += 1;
                continue;
            }
            b'#' => {
                while i < s.len() && s[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'\n' | b';' => {
                i += 1;
                Token::EndLine
            }
            b'\'' | b'"' => {
                let quote = s[i];
                i += 1;
                let mut bytes = Vec::new();
                let mut closed = false;
                while i < s.len() {
                    let b = s[i];
                    i += 1;
                    if b == quote {
                        closed = true;
                        break;
                    }
                    if quote == b'"' && b == b'#' && s.get(i) == Some(&b'{') {
                        return Err(Error::syntax(
                            i - 1,
                            "string interpolation is not implemented",
                        ));
                    }
                    if b != b'\\' {
                        bytes.push(b);
                        continue;
                    }
                    let Some(&escape) = s.get(i) else {
                        break;
                    };
                    i += 1;
                    if quote == b'\'' && escape != b'\'' && escape != b'\\' {
                        bytes.extend_from_slice(&[b'\\', escape]);
                        continue;
                    }
                    match escape {
                        b'n' => bytes.push(b'\n'),
                        b'r' => bytes.push(b'\r'),
                        b't' => bytes.push(b'\t'),
                        b'0' => bytes.push(0),
                        b'\\' | b'\'' | b'"' | b'#' => bytes.push(escape),
                        b'x' => {
                            let end = i + 2;
                            if end > s.len() {
                                return Err(Error::syntax(i, "incomplete hexadecimal escape"));
                            }
                            let hex = std::str::from_utf8(&s[i..end])
                                .map_err(|_| Error::syntax(i, "invalid hexadecimal escape"))?;
                            bytes.push(
                                u8::from_str_radix(hex, 16)
                                    .map_err(|_| Error::syntax(i, "invalid hexadecimal escape"))?,
                            );
                            i = end;
                        }
                        _ => return Err(Error::syntax(i - 1, "unsupported string escape")),
                    }
                }
                if !closed {
                    return Err(Error::syntax(start, "unterminated string"));
                }
                Token::Bytes(bytes)
            }
            b'0'..=b'9' => {
                i += 1;
                if s[start] == b'0'
                    && s.get(i)
                        .is_some_and(|b| matches!(b, b'x' | b'X' | b'b' | b'B' | b'o' | b'O'))
                {
                    let radix = match s[i] {
                        b'x' | b'X' => 16,
                        b'b' | b'B' => 2,
                        _ => 8,
                    };
                    i += 1;
                    let digits = i;
                    while i < s.len() && (s[i].is_ascii_alphanumeric() || s[i] == b'_') {
                        i += 1;
                    }
                    let text = &source[digits..i];
                    if text.is_empty()
                        || text.starts_with('_')
                        || text.ends_with('_')
                        || text.contains("__")
                    {
                        return Err(Error::syntax(start, "invalid integer literal"));
                    }
                    Token::Int(
                        i64::from_str_radix(&text.replace('_', ""), radix)
                            .map_err(|_| Error::syntax(start, "invalid or overflowing integer"))?,
                    )
                } else {
                    while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'_') {
                        i += 1;
                    }
                    let mut float = false;
                    if s.get(i) == Some(&b'.') && s.get(i + 1).is_some_and(u8::is_ascii_digit) {
                        float = true;
                        i += 1;
                        while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'_') {
                            i += 1;
                        }
                    }
                    if s.get(i).is_some_and(|b| matches!(b, b'e' | b'E')) {
                        float = true;
                        i += 1;
                        if s.get(i).is_some_and(|b| matches!(b, b'+' | b'-')) {
                            i += 1;
                        }
                        while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'_') {
                            i += 1;
                        }
                    }
                    if s.get(i)
                        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
                    {
                        return Err(Error::syntax(start, "invalid numeric literal"));
                    }
                    let raw = &source[start..i];
                    for (j, b) in raw.bytes().enumerate() {
                        if b == b'_'
                            && (j == 0
                                || j + 1 == raw.len()
                                || !raw.as_bytes()[j - 1].is_ascii_digit()
                                || !raw.as_bytes()[j + 1].is_ascii_digit())
                        {
                            return Err(Error::syntax(start, "invalid numeric separator"));
                        }
                    }
                    let text = raw.replace('_', "");
                    if float {
                        Token::Float(
                            text.parse()
                                .map_err(|_| Error::syntax(start, "invalid float"))?,
                        )
                    } else {
                        Token::Int(text.parse().map_err(|_| {
                            Error::syntax(start, "integer overflow is not implemented")
                        })?)
                    }
                }
            }
            b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                i += 1;
                while i < s.len() && (s[i].is_ascii_alphanumeric() || s[i] == b'_') {
                    i += 1;
                }
                if s.get(i) == Some(&b'?') {
                    i += 1;
                }
                Token::Word(source[start..i].to_owned())
            }
            _ => {
                let mut found = None;
                for op in [
                    "...", "..", "===", "||=", "&&=", "**=", "==", "!=", "<=", ">=", "&&", "||",
                    "+=", "-=", "*=", "/=", "%=", "**", "<<",
                ] {
                    if s[i..].starts_with(op.as_bytes()) {
                        found = Some(op);
                        break;
                    }
                }
                if let Some(op) = found {
                    i += op.len();
                    Token::Op(op)
                } else {
                    i += 1;
                    match s[start] {
                        b'+' => Token::Op("+"),
                        b'-' => Token::Op("-"),
                        b'*' => Token::Op("*"),
                        b'/' => Token::Op("/"),
                        b'%' => Token::Op("%"),
                        b'=' => Token::Op("="),
                        b'<' => Token::Op("<"),
                        b'>' => Token::Op(">"),
                        b'!' => Token::Op("!"),
                        b'(' | b')' | b'[' | b']' | b'{' | b'}' | b',' | b'.' | b':' | b'?' => {
                            Token::P(s[start] as char)
                        }
                        _ => return Err(Error::syntax(start, "unsupported character")),
                    }
                }
            }
        };
        out.push(Lexeme {
            token,
            offset: start,
        });
    }
    out.push(Lexeme {
        token: Token::Eof,
        offset: s.len(),
    });
    Ok(out)
}

#[derive(Debug)]
pub(crate) struct Expr {
    pub node: Node,
    depth: usize,
}
#[derive(Debug)]
pub(crate) enum Node {
    Literal(Value),
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
    Member(Box<Expr>, String),
    Method(Box<Expr>, String, Vec<Argument>),
    Index(Box<Expr>, Vec<Expr>),
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
}
impl Target {
    fn is_binding(&self) -> bool {
        match self {
            Self::Value(e) => matches!(e.node, Node::Var(_)),
            Self::Tuple(parts) => parts
                .iter()
                .all(|(target, _)| target.as_ref().is_none_or(Self::is_binding)),
        }
    }
    fn depth(&self) -> usize {
        match self {
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
}

pub(crate) fn parse(source: &str) -> Result<Vec<Definition>> {
    let mut p = Parser {
        tokens: lex(source)?,
        pos: 0,
        depth: 0,
        groups: 0,
    };
    let mut defs = Vec::new();
    let mut top = Vec::new();
    p.lines();
    while !matches!(p.token(), Token::Eof) {
        if p.word("def") {
            let name = p.name()?;
            let parenthesized = p.take_p('(');
            let params = p.parameters(parenthesized)?;
            p.lines();
            let body = p.block(&["end"])?;
            p.expect_word("end")?;
            if defs.iter().any(|d: &Definition| d.name == name) || name == "__main__" {
                return p.err("duplicate or reserved function name");
            }
            defs.push(Definition { name, params, body });
        } else {
            top.push(p.statement()?);
        }
        if !matches!(p.token(), Token::Eof | Token::EndLine) {
            return p.err("expected newline or semicolon");
        }
        p.lines();
    }
    defs.insert(
        0,
        Definition {
            name: "__main__".into(),
            params: Vec::new(),
            body: top,
        },
    );
    Ok(defs)
}

struct Parser {
    tokens: Vec<Lexeme>,
    pos: usize,
    depth: usize,
    groups: usize,
}
impl Parser {
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
            || (!parenthesized && matches!(self.token(), Token::EndLine))
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
            let default = if self.take_p(':') {
                if kind != ParamKind::Positional {
                    return self.err("capture type annotations are not implemented");
                }
                kind = ParamKind::Keyword;
                if parenthesized {
                    self.lines();
                }
                if matches!(self.token(), Token::P(',' | ')') | Token::EndLine) {
                    None
                } else {
                    let grouped = self.token() == &Token::P('(');
                    let value = self.expr(0)?;
                    if !grouped && annotation_expression(&value, &params, false) {
                        return self.err("type annotations are not implemented");
                    }
                    Some(value)
                }
            } else if self.token() == &Token::Op("=") {
                self.bump();
                if kind != ParamKind::Positional {
                    return self.err("capture parameters cannot have defaults");
                }
                if parenthesized {
                    self.lines();
                }
                Some(self.expr(0)?)
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
            params.push(Parameter {
                name,
                kind,
                default,
            });
            if parenthesized {
                self.lines();
                if self.take_p(')') {
                    self.groups -= 1;
                    break;
                }
            } else if matches!(self.token(), Token::EndLine | Token::Eof) {
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
    fn block(&mut self, stop: &[&str]) -> Result<Vec<Stmt>> {
        self.enter()?;
        let mut body = Vec::new();
        self.lines();
        while !matches!(self.token(),Token::Word(w) if stop.contains(&w.as_str())) {
            if matches!(self.token(), Token::Eof) {
                return self.err("unexpected end of source");
            }
            body.push(self.statement()?);
            if !self.at_end() && !matches!(self.token(), Token::EndLine) {
                return self.err("expected newline or semicolon");
            }
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
        let mut condition = self.expr(0)?;
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
                    Some(self.expr(0)?)
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
            let target = self.target(true)?;
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
            let first = self.expr(0)?;
            let rhs = if self.take_p(',') {
                let mut items = vec![first];
                loop {
                    self.lines();
                    items.push(self.expr(0)?);
                    if !self.take_p(',') {
                        break;
                    }
                }
                let depth = 1 + items.iter().map(|e| e.depth).max().unwrap_or(0);
                self.make(Node::Array(items), depth)?
            } else {
                first
            };
            return Ok(Stmt::Assign(target, op, rhs));
        }
        Ok(Stmt::Expr(self.expr(0)?))
    }
    fn negate(&self, expr: Expr) -> Result<Expr> {
        let depth = expr.depth + 1;
        self.make(Node::Unary("!", Box::new(expr)), depth)
    }
    fn while_stmt(&mut self, until: bool) -> Result<Stmt> {
        let mut cond = self.expr(0)?;
        if until {
            cond = self.negate(cond)?;
        }
        self.word("do");
        let body = self.block(&["end"])?;
        self.expect_word("end")?;
        Ok(Stmt::While(cond, body))
    }
    fn for_stmt(&mut self) -> Result<Stmt> {
        let target = self.target(false)?;
        if !target.is_binding() {
            return self.err("invalid for loop target");
        }
        self.expect_word("in")?;
        let iterable = self.expr(0)?;
        self.word("do");
        let body = self.block(&["end"])?;
        self.expect_word("end")?;
        Ok(Stmt::For(target, iterable, body))
    }
    fn target(&mut self, first_expression: bool) -> Result<Target> {
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
            let value = if rest
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
                    let inner = self.target(false)?;
                    self.lines();
                    self.expect_p(close)?;
                    Some(match inner {
                        Target::Tuple(_) => inner,
                        _ => Target::Tuple(vec![(Some(inner), false)]),
                    })
                } else {
                    Some(Target::Value(self.expr(0)?))
                }
            };
            parts.push((value, rest));
            if !self.take_p(',') {
                break;
            }
            tuple = true;
            self.lines();
            if matches!(self.token(), Token::P(')' | ']') | Token::Op("="))
                || matches!(self.token(), Token::Word(w) if w=="in")
            {
                break;
            }
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
        for (i, lexeme) in self.tokens[self.pos..].iter().enumerate() {
            match &lexeme.token {
                Token::P('(' | '[' | '{') => nesting += 1,
                Token::P(')' | ']' | '}') => {
                    if nesting == 0 {
                        return false;
                    }
                    nesting -= 1;
                }
                Token::Op(op) if nesting == 0 && assignment(op) => return true,
                Token::EndLine if nesting == 0 => {
                    let next = self.tokens[self.pos + i + 1..]
                        .iter()
                        .find(|l| !matches!(l.token, Token::EndLine));
                    if !comma
                        && !next.is_some_and(|l| matches!(l.token, Token::Op(op) if assignment(op)))
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
        let mut cond = self.expr(0)?;
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
        let mut cond = self.expr(0)?;
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
            Some(Box::new(self.expr(0)?))
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
                values.push((self.expr(0)?, splat));
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
    // Keep the prefix and tail frames separate so debug builds reach the nesting guard.
    fn prefix(&mut self) -> Result<Expr> {
        Ok(match self.bump() {
            Token::Int(n) => self.make(Node::Literal(Value::int(n)), 1)?,
            Token::Float(n) => self.make(Node::Literal(Value::float(n)), 1)?,
            Token::Bytes(b) => self.make(Node::Literal(Value::bytes(b)), 1)?,
            Token::Word(w) => match w.as_str() {
                "nil" => self.make(Node::Literal(Value::nil()), 1)?,
                "true" => self.make(Node::Literal(Value::boolean(true)), 1)?,
                "false" => self.make(Node::Literal(Value::boolean(false)), 1)?,
                "if" | "unless" => self.if_expr(w == "unless")?,
                "case" => self.case_expr()?,
                "while" | "until" | "for" => {
                    let stmt = if w == "for" {
                        self.for_stmt()?
                    } else {
                        self.while_stmt(w == "until")?
                    };
                    let depth = stmt.depth();
                    self.make(Node::Loop(Box::new(stmt)), depth)?
                }
                _ if reserved(&w) => return self.err("expected expression"),
                _ => self.make(Node::Var(w), 1)?,
            },
            Token::P(':') => {
                let bytes = match self.bump() {
                    Token::Word(w) => w.into_bytes(),
                    Token::Bytes(b) => b,
                    _ => return self.err("expected symbol"),
                };
                self.make(Node::Literal(Value::symbol(bytes)), 1)?
            }
            Token::P('(') => {
                self.groups += 1;
                self.lines();
                let e = self.expr(0)?;
                self.lines();
                self.expect_p(')')?;
                self.groups -= 1;
                e
            }
            Token::P('[') => {
                let a = self.arguments(']')?;
                let d = 1 + a.iter().map(|e| e.depth).max().unwrap_or(0);
                self.make(Node::Array(a), d)?
            }
            Token::P('{') => {
                self.groups += 1;
                let mut entries = Vec::new();
                self.lines();
                if !self.take_p('}') {
                    loop {
                        let key = match self.bump() {
                            Token::Word(w) => w.into_bytes(),
                            Token::Bytes(b) => b,
                            _ => return self.err("expected hash label"),
                        };
                        self.expect_p(':')?;
                        self.lines();
                        entries.push((key, self.expr(0)?));
                        self.lines();
                        if self.take_p('}') {
                            break;
                        }
                        self.expect_p(',')?;
                        self.lines();
                        if self.take_p('}') {
                            break;
                        }
                    }
                }
                let d = 1 + entries.iter().map(|(_, e)| e.depth).max().unwrap_or(0);
                self.groups -= 1;
                self.make(Node::Hash(entries), d)?
            }
            Token::Op(op @ (".." | "...")) => {
                if self.groups > 0 {
                    self.lines();
                }
                let end = self.expr(8)?;
                let depth = end.depth + 1;
                self.make(Node::Range(None, Some(Box::new(end)), op == "..."), depth)?
            }
            Token::Op(op @ ("-" | "+" | "!")) => {
                let e = self.expr(13)?;
                let d = e.depth + 1;
                self.make(Node::Unary(op, Box::new(e)), d)?
            }
            _ => return self.err("expected expression"),
        })
    }
    fn expr_tail(&mut self, mut lhs: Expr, min: u8) -> Result<Expr> {
        loop {
            if self.take_p('(') {
                let Node::Var(name) = lhs.node else {
                    return self.err("only named functions are callable");
                };
                let args = self.call_arguments()?;
                let d = 1 + args.iter().map(|a| a.value.depth).max().unwrap_or(0);
                lhs = self.make(Node::Call(name, args), d)?;
                continue;
            }
            if self.take_p('.') {
                let Token::Word(name) = self.bump() else {
                    return self.err("expected member name");
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
                let yes = self.expr(0)?;
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
            let Token::Op(op) = self.token() else {
                break;
            };
            let op = *op;
            let (left, right) = match op {
                "||" => (3, 4),
                "&&" => (4, 5),
                "==" | "!=" | "===" => (5, 6),
                "<" | "<=" | ">" | ">=" => (6, 7),
                ".." | "..." => (7, 8),
                "<<" => (10, 11),
                "+" | "-" => (11, 12),
                "*" | "/" | "%" => (12, 13),
                "**" => (14, 14),
                _ => break,
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
            self.lines();
            let rhs = self.expr(right)?;
            let depth = 1 + lhs.depth.max(rhs.depth);
            lhs = self.make(Node::Binary(op, Box::new(lhs), Box::new(rhs)), depth)?;
        }
        Ok(lhs)
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
        self.lines();
        if self.take_p(')') {
            self.groups -= 1;
            return Ok(args);
        }
        loop {
            let kind = if self.token() == &Token::Op("**") {
                self.bump();
                ArgumentKind::KeywordSplat
            } else if matches!(self.token(), Token::Word(_))
                && self
                    .tokens
                    .get(self.pos + 1)
                    .is_some_and(|t| t.token == Token::P(':'))
            {
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
            };
            let keyword = matches!(kind, ArgumentKind::Keyword(_) | ArgumentKind::KeywordSplat);
            if keywords && !keyword {
                return self.err("positional arguments cannot follow keywords");
            }
            keywords |= keyword;
            self.lines();
            let value = if let ArgumentKind::Keyword(name) = &kind {
                if matches!(self.token(), Token::P(',' | ')')) {
                    self.make(Node::Var(name.clone()), 1)?
                } else {
                    self.expr(0)?
                }
            } else {
                self.expr(0)?
            };
            args.push(Argument { kind, value });
            self.lines();
            if self.take_p(')') {
                break;
            }
            self.expect_p(',')?;
            self.lines();
            if self.take_p(')') {
                break;
            }
        }
        self.groups -= 1;
        Ok(args)
    }
    fn starts_expression(&self) -> bool {
        match self.token() {
            Token::Word(w) => {
                !reserved(w)
                    || matches!(
                        w.as_str(),
                        "if" | "unless" | "case" | "while" | "until" | "for"
                    )
            }
            Token::Int(_) | Token::Float(_) | Token::Bytes(_) => true,
            Token::P('(' | '[' | '{' | ':') | Token::Op("+" | "-" | "!") => true,
            _ => false,
        }
    }
}

fn annotation_expression(expr: &Expr, params: &[Parameter], shape: bool) -> bool {
    match &expr.node {
        Node::Var(name) => !shape || !params.iter().any(|p| p.name == *name),
        Node::Member(root, _) => annotation_expression(root, params, true),
        Node::Hash(fields) => {
            !fields.is_empty()
                && fields
                    .iter()
                    .all(|(_, value)| annotation_expression(value, params, true))
        }
        _ => false,
    }
}

fn assignment(op: &str) -> bool {
    matches!(
        op,
        "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "**=" | "||=" | "&&="
    )
}

fn reserved(w: &str) -> bool {
    matches!(
        w,
        "class"
            | "module"
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
pub(crate) fn unsupported(message: &str) -> Error {
    Error::new(crate::ErrorKind::Syntax, message)
}
