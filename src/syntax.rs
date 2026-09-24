use crate::{
    Error, Result, Value,
    compilation::{Boxed, Buffer, Bytes, Name, Table, Task, Tasks, Text, Work},
};
use std::cell::{RefCell, RefMut};

mod classes;
mod errors;
mod lexer;
pub(crate) mod modules;
pub(crate) mod record;
mod teardown;
mod tokens;
mod types;
pub(crate) mod unicode;
mod work;
use lexer::{Lexeme, Part, Token, lex};
use tokens::Tokens;

// Go's maxSyntaxDepth bounds both parser recursion and syntax tree height.
const MAX_DEPTH: usize = 1024;
pub(crate) const MAX_SOURCE: usize = 8 << 20;
pub(crate) const TOO_DEEP: &str = "syntax nesting too deep";

#[derive(Debug)]
pub(crate) struct Expr {
    pub node: Node,
    depth: u32,
    pub offset: u32,
}
#[derive(Debug)]
pub(crate) enum Node {
    Try(Boxed<Try>),
    Regex(Bytes, u8),
    Shape(
        Boxed<crate::compilation::Type>,
        Option<Boxed<Expr>>,
        Buffer<Name>,
    ),
    Integer(u64),
    BigInteger(Text, u32),
    Literal(Value),
    Template(Buffer<Expr>, bool),
    Var(Name),
    Array(Buffer<Expr>),
    Hash(Buffer<(Bytes, Expr)>),
    Unary(&'static str, Boxed<Expr>),
    Binary(&'static str, Boxed<Expr>, Boxed<Expr>),
    Range(Option<Boxed<Expr>>, Option<Boxed<Expr>>, bool),
    /// Conditions tested in order with their results, then the alternate.
    Conditional(Buffer<(Expr, Expr)>, Boxed<Expr>),
    Case(Option<Boxed<Expr>>, Buffer<When>, Option<Boxed<Expr>>),
    /// A loop, or an if continued past its `end`, used as an expression.
    Compound(Boxed<Stmt>),
    Call(Name, Buffer<Argument>, CallForm),
    ComputedCall(Boxed<Expr>, Buffer<Argument>),
    BlockCall(Boxed<Expr>, Block),
    Yield(Buffer<Expr>),
    Member(Boxed<Expr>, Name),
    SafeMember(Boxed<Expr>, Name),
    Scope(Boxed<Expr>, Name, Option<Buffer<Argument>>),
    Method(Boxed<Expr>, Name, Buffer<Argument>, CallForm),
    SafeMethod(Boxed<Expr>, Name, Buffer<Argument>, CallForm),
    Index(Boxed<Expr>, Buffer<Expr>),
}
impl Expr {
    /// Moves the node out of an expression, which cannot be destructured
    /// because it drops its subtree without recursion.
    fn into_node(mut self) -> Node {
        std::mem::replace(&mut self.node, Node::Integer(0))
    }

    /// The offset of the first safe navigation in an assignment target's
    /// receiver chain, which Go reports there.
    fn safe_navigation(&self) -> Option<usize> {
        let mut current = self;
        loop {
            current = match &current.node {
                Node::SafeMember(..) | Node::SafeMethod(..) => {
                    return Some(current.offset as usize);
                }
                Node::Member(receiver, _)
                | Node::Method(receiver, _, _, _)
                | Node::Index(receiver, _)
                | Node::ComputedCall(receiver, _)
                | Node::BlockCall(receiver, _) => receiver,
                _ => return None,
            };
        }
    }

    /// Reports whether Go's parser accepts the expression as an assignment target.
    fn assignable(&self) -> bool {
        match &self.node {
            Node::Var(_) => true,
            Node::Member(..) | Node::Index(..) => self.safe_navigation().is_none(),
            _ => false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CallForm {
    Auto,
    Bare,
    Parenthesized,
}
enum Suffix {
    Rescue,
    Command,
    Block(bool),
    Call,
    Scope,
    Member(bool),
    Index(u32),
    Ternary(u32),
    Binary(&'static str, u8, u32),
}
#[derive(Debug)]
pub(crate) struct Try {
    pub modifier: bool,
    pub body: Buffer<Stmt>,
    pub rescues: Buffer<Rescue>,
    pub alternate: Buffer<Stmt>,
    pub ensure: Buffer<Stmt>,
}
#[derive(Debug)]
pub(crate) struct Rescue {
    pub classes: Buffer<crate::ErrorClass>,
    pub binding: Option<Name>,
    pub body: Buffer<Stmt>,
    pub offset: u32,
}
impl Try {
    fn depth(&self) -> u32 {
        let depth = self
            .body
            .iter()
            .chain(&self.alternate)
            .chain(&self.ensure)
            .chain(self.rescues.iter().flat_map(|r| &r.body))
            .map(|stmt| stmt.depth)
            .max()
            .unwrap_or(0);
        // A rescue modifier wraps its two expressions rather than statements.
        if self.modifier { depth } else { 1 + depth }
    }
}
#[derive(Debug)]
pub(crate) struct Block {
    /// The offset of the opening `do` or `{`.
    pub offset: u32,
    pub params: Buffer<Target>,
    pub body: Buffer<Stmt>,
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
    pub ivar: Option<Name>,
    pub name: Name,
    pub kind: ParamKind,
    pub default: Option<Expr>,
    pub ty: Option<crate::compilation::Type>,
}
#[derive(Debug)]
pub(crate) enum ArgumentKind {
    Positional,
    Splat,
    Keyword(Name),
    KeywordSplat,
}
#[derive(Debug)]
pub(crate) struct Argument {
    pub kind: ArgumentKind,
    pub value: Expr,
}
#[derive(Debug)]
pub(crate) struct When {
    pub values: Buffer<(Expr, bool)>,
    pub result: Expr,
}
#[derive(Debug)]
pub(crate) enum Target {
    Value(Expr),
    Tuple(Buffer<(Option<Target>, bool)>),
    Typed(Boxed<Target>, crate::compilation::Type),
}
impl Target {
    pub fn offset(&self) -> Option<u32> {
        let mut target = self;
        loop {
            target = match target {
                Self::Value(expr) => return Some(expr.offset),
                Self::Typed(target, _) => target,
                Self::Tuple(parts) => parts.iter().find_map(|(target, _)| target.as_ref())?,
            };
        }
    }
    // Destructuring nests as deeply as the syntax limit, so walk it without recursion.
    fn parts(&self, mut visit: impl FnMut(&Self, u32) -> bool) -> bool {
        let mut pending = vec![(self, 0)];
        while let Some((target, depth)) = pending.pop() {
            if !visit(target, depth) {
                return false;
            }
            match target {
                Self::Typed(target, _) => pending.push((target, depth)),
                Self::Value(_) => (),
                Self::Tuple(parts) => {
                    pending.extend(
                        parts
                            .iter()
                            .rev()
                            .filter_map(|(t, _)| t.as_ref())
                            .map(|t| (t, depth + 1)),
                    );
                }
            }
        }
        true
    }
    fn is_binding(&self) -> bool {
        self.parts(|target, _| match target {
            Self::Value(e) => matches!(&e.node, Node::Var(name) if !name.starts_with('@')),
            _ => true,
        })
    }
    fn depth(&self) -> u32 {
        let mut deepest = 0;
        self.parts(|target, depth| {
            if let Self::Value(e) = target {
                deepest = deepest.max(depth + e.depth);
            } else if let Self::Tuple(_) = target {
                deepest = deepest.max(depth + 1);
            }
            true
        });
        deepest
    }
}
#[derive(Debug)]
pub(crate) struct Stmt {
    pub node: Statement,
    depth: u32,
    pub offset: u32,
}
#[derive(Debug)]
pub(crate) enum Statement {
    Raise(Option<Boxed<Expr>>, Option<Boxed<Expr>>),
    Retry,
    Module(Name),
    UnboundClass(Name),
    Expr(Expr),
    Assign(Target, &'static str, Expr),
    /// Conditions tested in order with their bodies, then the alternate. A
    /// modifier, whose body precedes its condition in the source, records the
    /// offset of its keyword.
    If(Buffer<(Expr, Buffer<Stmt>)>, Buffer<Stmt>, Option<u32>),
    /// A modifier, whose body precedes its condition, records its keyword offset.
    While(Expr, Buffer<Stmt>, Option<u32>),
    For(Target, Expr, Buffer<Stmt>),
    Return(Option<Expr>),
    Break(Option<Expr>),
    Next(Option<Expr>),
}
impl Statement {
    fn at(self, offset: u32) -> Stmt {
        Stmt {
            depth: self.depth(),
            node: self,
            offset,
        }
    }
    // Match Go's syntax tree height; children carry their own heights.
    fn depth(&self) -> u32 {
        let body = |s: &[Stmt]| s.iter().map(|s| s.depth).max().unwrap_or(0);
        match self {
            Statement::Module(_) | Statement::UnboundClass(_) | Statement::Retry => 1,
            Statement::Raise(value, message) => {
                1 + value
                    .iter()
                    .chain(message)
                    .map(|v| v.depth)
                    .max()
                    .unwrap_or(0)
            }
            // A statement-position begin is a statement in Go, not an expression statement.
            Statement::Expr(Expr {
                node: Node::Try(attempt),
                depth,
                ..
            }) if !attempt.modifier => *depth,
            Statement::Expr(e) => 1 + e.depth,
            Statement::Assign(t, _, e) => 1 + t.depth().max(e.depth),
            Statement::If(branches, alternate, _) => {
                let branches = branches.iter().enumerate().map(|(i, (condition, body_))| {
                    // Each elsif is its own node beside the first branch.
                    u32::from(i > 0) + condition.depth.max(body(body_))
                });
                1 + branches.max().unwrap_or(0).max(body(alternate))
            }
            Statement::While(e, b, _) => 1 + e.depth.max(body(b)),
            Statement::For(t, e, b) => 1 + t.depth().max(e.depth).max(body(b)),
            Statement::Return(e) | Statement::Break(e) | Statement::Next(e) => {
                1 + e.as_ref().map_or(0, |e| e.depth)
            }
        }
    }
}
#[derive(Debug)]
pub(crate) struct Definition {
    pub offset: u32,
    pub private: bool,
    pub accessor: Option<(Name, bool)>,
    pub name: Name,
    pub params: Buffer<Parameter>,
    pub body: Buffer<Stmt>,
    pub return_type: Option<crate::compilation::Type>,
}
impl Definition {
    fn depth(&self) -> u32 {
        let params = self
            .params
            .iter()
            .map(|p| p.default.as_ref().map_or(1, |e| e.depth))
            .max()
            .unwrap_or(0);
        1 + self
            .body
            .iter()
            .map(|s| s.depth)
            .max()
            .unwrap_or(0)
            .max(params)
    }
}

pub(crate) struct Declarations {
    pub functions: Buffer<Definition>,
    pub enums: Buffer<(Name, Buffer<Name>)>,
    pub modules: Buffer<modules::Module>,
    pub outline: Buffer<Outline>,
    /// The byte span of every string interpolation's content, for tooling
    /// that reports positions relative to an interpolation as Go does.
    pub interpolations: Buffer<(u32, u32)>,
}

/// A top-level declaration's kind, name and source byte range, in source order.
pub(crate) struct Outline {
    pub kind: crate::DeclarationKind,
    pub name: Name,
    pub start: usize,
    pub end: usize,
}

fn parser<'a>(source: &'a str, work: &'a dyn crate::compilation::Work) -> Result<Parser<'a>> {
    Ok(Parser {
        work,
        source,
        lex_depth: 0,
        tokens: Tokens::new(lex(source, work)?, work)?,
        pos: 0,
        depth: 0,
        groups: 0,
        line_exprs: 0,
        command_depth: 0,
        ternaries: Buffer::new(),
        command_group: 0,
        loop_condition: None,
        then_stop: None,
        locals: Table::new(),
        declared_it: false,
        type_structural_error: false,
        interpolations: Buffer::new(),
        record: None,
    })
}

pub(crate) fn parse_type(source: &str) -> Result<crate::types::Type> {
    let mut p = parser(source, &())?;
    let ty = p.type_expr(1, false)?;
    p.line_breaks()?;
    if !matches!(p.token(), Token::Eof) {
        return p.err("unexpected trailing input in type annotation");
    }
    ty.compile(&())
}

pub(crate) fn parse(source: &str, work: &dyn crate::compilation::Work) -> Result<Declarations> {
    let parsing = Parsing::new(parser(source, work)?);
    match parsing.run(Call::Program)? {
        Parsed::Program(declarations) => Ok(declarations),
        _ => unreachable!(),
    }
}

struct Parser<'a> {
    work: &'a dyn crate::compilation::Work,
    source: &'a str,
    lex_depth: usize,
    tokens: Tokens<'a>,
    pos: usize,
    depth: usize,
    groups: usize,
    line_exprs: usize,
    command_depth: usize,
    ternaries: Buffer<usize>,
    command_group: usize,
    loop_condition: Option<usize>,
    then_stop: Option<usize>,
    locals: Table<()>,
    declared_it: bool,
    type_structural_error: bool,
    interpolations: Buffer<(u32, u32)>,
    /// Tooling facts, collected only by [`record::parse`].
    record: Option<Box<record::Record>>,
}

/// Recursive parsing steps that run as tasks instead of native calls.
enum Call {
    Program,
    Interpolation,
    Expr(u8),
    /// The rest of an expression whose first operand needed no nested task.
    Tail(Expr, Suffix, u8),
    Block(&'static [&'static str]),
    Target(bool),
    Class,
    Module,
}

enum Parsed {
    Program(Declarations),
    Expr(Expr),
    Body(Buffer<Stmt>),
    /// A target, and whether commas at its own level made it a tuple.
    Target(Target, bool),
    Module(modules::Module),
}

// The first operand of an expression, parsed without a task when it is a
// single token.
enum Leaf {
    Done(Expr),
    Tail(Expr, Suffix),
    Nested,
}

/// Runs the recursive grammar over shared parser state, so source nesting
/// grows a heap task stack instead of the native one.
struct Parsing<'a> {
    parser: RefCell<Parser<'a>>,
    tasks: Tasks<Call, Parsed>,
}

impl<'a> Parsing<'a> {
    fn new(parser: Parser<'a>) -> Self {
        Self {
            parser: RefCell::new(parser),
            tasks: Tasks::new(),
        }
    }

    fn p(&self) -> RefMut<'_, Parser<'a>> {
        self.parser.borrow_mut()
    }

    fn run(&self, call: Call) -> Result<Parsed> {
        self.tasks.run(call, |call| self.start(call))
    }

    fn start(&self, call: Call) -> Task<'_, Parsed> {
        match call {
            Call::Program => Box::pin(async { Ok(Parsed::Program(self.program().await?)) }),
            Call::Interpolation => Box::pin(async { Ok(Parsed::Expr(self.interpolated().await?)) }),
            Call::Expr(min) => {
                Box::pin(async move { Ok(Parsed::Expr(self.expression(min).await?)) })
            }
            Call::Tail(lhs, suffix, min) => Box::pin(async move {
                let result = self.expr_tail(lhs, min, Some(suffix)).await;
                self.p().depth -= 1;
                Ok(Parsed::Expr(result?))
            }),
            Call::Block(stop) => {
                Box::pin(async move { Ok(Parsed::Body(self.block_task(stop).await?)) })
            }
            Call::Target(typed) => Box::pin(async move {
                let (target, tuple) = self.target(false, typed).await?;
                Ok(Parsed::Target(target, tuple))
            }),
            Call::Class => Box::pin(async { Ok(Parsed::Module(self.class().await?)) }),
            Call::Module => Box::pin(async { Ok(Parsed::Module(self.module().await?)) }),
        }
    }

    async fn expr(&self, min: u8) -> Result<Expr> {
        let call = match self.p().leaf_expression(min)? {
            Leaf::Done(expr) => return Ok(expr),
            Leaf::Tail(lhs, suffix) => Call::Tail(lhs, suffix, min),
            Leaf::Nested => Call::Expr(min),
        };
        match self.tasks.call(call).await? {
            Parsed::Expr(expr) => Ok(expr),
            _ => unreachable!(),
        }
    }

    async fn line_expr(&self, min: u8) -> Result<Expr> {
        {
            let mut p = self.p();
            p.work.charge(1)?;
            p.line_exprs += 1;
        }
        let result = self.expr(min).await;
        self.p().line_exprs -= 1;
        result
    }

    async fn block(&self, stop: &'static [&'static str]) -> Result<Buffer<Stmt>> {
        match self.tasks.call(Call::Block(stop)).await? {
            Parsed::Body(body) => Ok(body),
            _ => unreachable!(),
        }
    }

    async fn nested_target(&self, typed: bool) -> Result<(Target, bool)> {
        match self.tasks.call(Call::Target(typed)).await? {
            Parsed::Target(target, tuple) => Ok((target, tuple)),
            _ => unreachable!(),
        }
    }

    async fn nested_class(&self) -> Result<modules::Module> {
        match self.tasks.call(Call::Class).await? {
            Parsed::Module(class) => Ok(class),
            _ => unreachable!(),
        }
    }

    async fn nested_module(&self) -> Result<modules::Module> {
        match self.tasks.call(Call::Module).await? {
            Parsed::Module(module) => Ok(module),
            _ => unreachable!(),
        }
    }

    async fn program(&self) -> Result<Declarations> {
        let work = self.p().work;
        let mut defs = Buffer::new();
        // Indexes top-level functions by name so duplicate checks stay linear.
        let mut def_names: Table<usize> = Table::new();
        let mut enums = Buffer::new();
        let mut modules = Buffer::new();
        let mut top = Buffer::new();
        let mut outline = Buffer::new();
        self.p().lines()?;
        while !matches!(self.p().token(), Token::Eof) {
            let (offset, first) = {
                let p = self.p();
                (p.tokens[p.pos].offset as u32, p.pos)
            };
            let class = self.p().word("class");
            if class || self.p().module_ahead() {
                // Go parses each top-level declaration as a statement.
                self.p().enter()?;
                let module = if class {
                    self.class().await?
                } else {
                    self.module().await?
                };
                self.p().depth -= 1;
                let kind = if class {
                    crate::DeclarationKind::Class
                } else {
                    crate::DeclarationKind::Module
                };
                let end = self.p().declaration_end(first);
                outline.push(
                    work,
                    Outline {
                        kind,
                        name: module.name.clone(),
                        start: offset as usize,
                        end,
                    },
                )?;
                top.push(work, Statement::Module(module.name.clone()).at(offset))?;
                let index = modules.len();
                modules.push(work, module)?;
                self.p()
                    .note(|record| record.top.push((offset, record::Top::Module(index))));
            } else if matches!(self.p().token(), Token::Word(word) if matches!(word.as_str(), "def" | "private" | "export"))
            {
                let name = {
                    let mut p = self.p();
                    p.enter()?;
                    let private = p.word("private");
                    if !private {
                        p.word("export");
                    }
                    p.expect_word("def")?;
                    let name = p.name()?;
                    if name.starts_with('@') {
                        return p.err("expected function name");
                    }
                    if def_names.contains(work, &name)? || name == "__main__" {
                        return p.err("duplicate or reserved function name");
                    }
                    (name, private)
                };
                let (name, private) = name;
                let mut definition = self.definition(name, offset).await?;
                definition.private = private;
                let mut p = self.p();
                p.check_depth(definition.depth())?;
                p.depth -= 1;
                outline.push(
                    work,
                    Outline {
                        kind: crate::DeclarationKind::Function,
                        name: definition.name.clone(),
                        start: offset as usize,
                        end: p.declaration_end(first),
                    },
                )?;
                let index = defs.len();
                def_names.insert(work, definition.name.clone(), index)?;
                defs.push(work, definition)?;
                p.note(|record| record.top.push((offset, record::Top::Function(index))));
            } else if self.p().alias_ahead() {
                // Go resolves a top-level alias against the functions declared before it.
                let mut p = self.p();
                let (name, target) = p.alias_names()?;
                if def_names.contains(work, &name)? || name == "__main__" {
                    return p.err("duplicate or reserved function name");
                }
                let Some(&original) = def_names.get(work, &target)? else {
                    return p.err("alias target function is not defined");
                };
                let original = &defs[original];
                let mut definition = work::definition(work, original)?;
                outline.push(
                    work,
                    Outline {
                        kind: crate::DeclarationKind::Function,
                        name: name.clone(),
                        start: offset as usize,
                        end: p.declaration_end(first),
                    },
                )?;
                definition.name = name;
                let index = defs.len();
                def_names.insert(work, definition.name.clone(), index)?;
                defs.push(work, definition)?;
                p.note(|record| {
                    let alias = record::Top::Alias(index, target.to_string());
                    record.top.push((offset, alias));
                });
            } else if self.p().word("enum") {
                let mut p = self.p();
                p.line_breaks()?;
                let name = p.enum_name()?;
                let mut members = Buffer::new();
                let mut member_offsets = Vec::new();
                let mut seen = Table::new();
                p.lines()?;
                while !matches!(p.token(), Token::Eof)
                    && !matches!(p.token(), Token::Word(w) if w == "end")
                {
                    if p.record.is_some() {
                        member_offsets.push(p.tokens[p.pos].offset as u32);
                    }
                    let member = if p.word("enum") {
                        Name::new(work, "enum")?
                    } else {
                        p.enum_name()?
                    };
                    if seen.insert(work, member.clone(), ())?.is_some() {
                        return p.err("duplicate enum member");
                    }
                    members.push(work, member)?;
                    p.lines()?;
                }
                if members.is_empty() {
                    return p.err("enum must define at least one member");
                }
                p.expect_word("end")?;
                outline.push(
                    work,
                    Outline {
                        kind: crate::DeclarationKind::Enum,
                        name: name.clone(),
                        start: offset as usize,
                        end: p.declaration_end(first),
                    },
                )?;
                let index = enums.len();
                enums.push(work, (name, members))?;
                p.note(|record| {
                    record.top.push((offset, record::Top::Enum(index)));
                    record.enums.push(member_offsets);
                });
            } else {
                let index = top.len();
                top.push(work, self.statement().await?)?;
                self.p()
                    .note(|record| record.top.push((offset, record::Top::Statement(index))));
            }
            self.p().lines()?;
        }
        defs.insert(
            work,
            0,
            Definition {
                offset: 0,
                private: true,
                accessor: None,
                name: Name::new(work, "__main__")?,
                params: Buffer::new(),
                body: top,
                return_type: None,
            },
        )?;
        let interpolations = std::mem::take(&mut self.p().interpolations);
        Ok(Declarations {
            functions: defs,
            enums,
            modules,
            outline,
            interpolations,
        })
    }

    async fn parameters(&self, parenthesized: bool) -> Result<Buffer<Parameter>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut params = Buffer::new();
        let mut rest = false;
        let mut keywords = false;
        let mut keyword_rest = false;
        {
            let mut p = self.p();
            if parenthesized {
                p.groups += 1;
                p.lines()?;
            }
            // Without parentheses, a signature needs a parameter on the def's
            // line; anything else starts the body, as in `def run [1, 2].first end`.
            let bare = match p.token() {
                Token::Word(w) => !keyword(w) && !w.starts_with("@@"),
                Token::Op("*" | "**" | "&") => true,
                _ => false,
            };
            if (parenthesized && p.take_p(')')) || (!parenthesized && !bare) {
                if parenthesized {
                    p.groups -= 1;
                }
                return Ok(params);
            }
        }
        loop {
            let (name, instance, mut kind) = self.p().parameter_name()?;
            let mut ty = None;
            let colon = self.p().take_p(':');
            let default = if colon {
                if parenthesized {
                    self.p().line_breaks()?;
                }
                let bare_keyword = !instance
                    && kind == ParamKind::Positional
                    && matches!(
                        self.p().token(),
                        Token::P(',' | ')') | Token::EndLine | Token::Op("->")
                    );
                if bare_keyword {
                    kind = ParamKind::Keyword;
                    None
                } else if !instance
                    && kind == ParamKind::Positional
                    && self.p().keyword_default(parenthesized)?
                {
                    kind = ParamKind::Keyword;
                    Some(if parenthesized {
                        self.expr(0).await?
                    } else {
                        self.line_expr(0).await?
                    })
                } else {
                    let defaulted = {
                        let mut p = self.p();
                        let annotation = p.type_expr(1, false)?;
                        if matches!(kind, ParamKind::Rest | ParamKind::KeywordRest)
                            && !annotation.captures(kind == ParamKind::KeywordRest)
                        {
                            return p.err("capture annotation must accept its collection type");
                        }
                        ty = Some(annotation);
                        if kind == ParamKind::Positional && p.take_p(':') {
                            kind = ParamKind::Keyword;
                            if !matches!(
                                p.token(),
                                Token::P(',' | ')') | Token::EndLine | Token::Op("->")
                            ) {
                                return p
                                    .err("typed required keyword must end after trailing colon");
                            }
                        }
                        let defaulted = p.token() == &Token::Op("=");
                        if defaulted {
                            if kind != ParamKind::Positional {
                                return p.err("capture parameters cannot have defaults");
                            }
                            p.bump()?;
                            if parenthesized {
                                p.line_breaks()?;
                            }
                        }
                        defaulted
                    };
                    if defaulted {
                        Some(if parenthesized {
                            self.expr(0).await?
                        } else {
                            self.line_expr(0).await?
                        })
                    } else {
                        None
                    }
                }
            } else if self.p().token() == &Token::Op("=") {
                {
                    let mut p = self.p();
                    p.bump()?;
                    if kind != ParamKind::Positional {
                        return p.err("capture parameters cannot have defaults");
                    }
                    if parenthesized {
                        p.lines()?;
                    }
                }
                Some(if parenthesized {
                    self.expr(0).await?
                } else {
                    self.line_expr(0).await?
                })
            } else {
                None
            };
            let mut p = self.p();
            match kind {
                ParamKind::Positional if rest || keywords || keyword_rest => {
                    return p.err("positional parameters must precede rest and keyword parameters");
                }
                ParamKind::Rest => {
                    if rest || keywords || keyword_rest {
                        return p.err("invalid rest parameter order");
                    }
                    rest = true;
                }
                ParamKind::Keyword => {
                    if keyword_rest {
                        return p.err("keyword parameter follows keyword rest");
                    }
                    keywords = true;
                }
                ParamKind::KeywordRest => {
                    if keyword_rest {
                        return p.err("duplicate keyword rest parameter");
                    }
                    keyword_rest = true;
                }
                _ => (),
            }
            p.locals.insert(work, name.clone(), ())?;
            p.declared_it |= name == "it";
            params.push(
                work,
                Parameter {
                    ivar: instance.then(|| name.clone()),
                    name,
                    kind,
                    default,
                    ty,
                },
            )?;
            if parenthesized {
                p.lines()?;
                if p.take_p(')') {
                    p.groups -= 1;
                    break;
                }
            } else if p.token() != &Token::P(',') {
                break;
            }
            p.expect_p(',')?;
            if parenthesized {
                p.lines()?;
            }
        }
        Ok(params)
    }

    async fn block_task(&self, stop: &[&str]) -> Result<Buffer<Stmt>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut body = Buffer::new();
        self.p().lines()?;
        loop {
            {
                let p = self.p();
                if matches!(p.token(),Token::Word(w) if stop.contains(&w.as_str()))
                    || (p.token() == &Token::P('}') && stop.contains(&"}"))
                {
                    break;
                }
                if matches!(p.token(), Token::Eof) {
                    return p.expected(if stop.contains(&"}") {
                        Label::Char('}')
                    } else {
                        Label::Text("end")
                    });
                }
            }
            body.push(work, self.statement().await?)?;
            self.p().lines()?;
        }
        Ok(body)
    }

    // A condition ends at `then`, which otherwise names a local like any
    // identifier, as in Go.
    async fn condition(&self) -> Result<Expr> {
        let previous = {
            let mut p = self.p();
            let groups = p.groups;
            p.then_stop.replace(groups)
        };
        let condition = self.line_expr(0).await;
        self.p().then_stop = previous;
        condition
    }

    async fn statement(&self) -> Result<Stmt> {
        let offset = {
            let mut p = self.p();
            p.work.charge(1)?;
            p.enter()?;
            p.tokens[p.pos].offset as u32
        };
        let stmt = self.modified_statement(offset).await?.at(offset);
        let mut p = self.p();
        p.check_depth(stmt.depth)?;
        p.depth -= 1;
        Ok(stmt)
    }

    async fn modified_statement(&self, offset: u32) -> Result<Statement> {
        self.p().work.charge(1)?;
        let starts_begin = matches!(self.p().token(), Token::Word(word) if word == "begin");
        let mut stmt = if starts_begin {
            Box::pin(self.begin_statement()).await?
        } else {
            self.plain_statement().await?
        };
        if matches!(
            stmt,
            Statement::If(..) | Statement::While(..) | Statement::For(..)
        ) {
            stmt = Box::pin(self.continued_statement(stmt, offset)).await?;
        }
        let modifier = match self.p().token() {
            Token::Word(w) if matches!(w.as_str(), "if" | "unless" | "while" | "until") => *w,
            _ => return Ok(stmt),
        };
        let bare_begin = starts_begin
            && matches!(&stmt, Statement::Expr(Expr { node: Node::Try(attempt), .. }) if !attempt.modifier);
        if bare_begin
            || !matches!(
                stmt,
                Statement::Expr(_)
                    | Statement::Raise(..)
                    | Statement::Retry
                    | Statement::Assign(..)
                    | Statement::Return(_)
                    | Statement::Break(_)
                    | Statement::Next(_)
            )
        {
            return self
                .p()
                .err("modifier requires an expression, assignment, or leaf control statement");
        }
        let keyword = {
            let mut p = self.p();
            let keyword = p.tokens[p.pos].offset as u32;
            p.bump()?;
            keyword
        };
        let mut condition = self.line_expr(0).await?;
        let p = self.p();
        let work = p.work;
        if matches!(modifier.as_str(), "unless" | "until") {
            condition = p.negate(condition)?;
        }
        let body = Buffer::from_array(work, [stmt.at(offset)])?;
        Ok(if matches!(modifier.as_str(), "while" | "until") {
            Statement::While(condition, body, Some(keyword))
        } else {
            Statement::If(
                Buffer::from_array(work, [(condition, body)])?,
                Buffer::new(),
                Some(keyword),
            )
        })
    }

    // Like Go, an operator or member access after a compound statement's
    // `end` on the same line continues the statement as an expression.
    async fn continued_statement(&self, stmt: Statement, offset: u32) -> Result<Statement> {
        let expr = {
            let mut p = self.p();
            p.work.charge(1)?;
            let next = &p.tokens[p.pos];
            let continues = match next.token {
                Token::P('.' | '(' | '[' | '{' | '?') | Token::Op("&." | "::") => true,
                Token::Op(op) => binding_power(op).is_some(),
                Token::Word(ref w) => matches!(w.as_str(), "do" | "rescue"),
                _ => false,
            };
            if !continues || next.line != p.previous()?.end_line {
                return Ok(stmt);
            }
            let stmt = stmt.at(offset);
            let depth = stmt.depth;
            p.line_exprs += 1;
            p.make_at(Node::Compound(Boxed::new(p.work, stmt)?), depth, offset)?
        };
        let result = self.expr_tail(expr, 0, None).await;
        self.p().line_exprs -= 1;
        Ok(Statement::Expr(result?))
    }

    // Go parses a statement-position begin as a statement, which costs no
    // expression nesting, and then continues any expression after its end.
    async fn begin_statement(&self) -> Result<Statement> {
        let offset = {
            let mut p = self.p();
            let offset = p.tokens[p.pos].offset as u32;
            p.pos += 1;
            p.line_exprs += 1;
            offset
        };
        let result = async {
            let mut attempt = self.begin_expression().await?;
            attempt.offset = offset;
            self.expr_tail(attempt, 0, None).await
        }
        .await;
        self.p().line_exprs -= 1;
        Ok(Statement::Expr(result?))
    }

    async fn plain_statement(&self) -> Result<Statement> {
        let keyword = {
            let mut p = self.p();
            p.work.charge(1)?;
            if p.alias_ahead() {
                return p.err(
                    "alias declarations are only supported at the top level or in class bodies",
                );
            }
            let keyword = [
                "raise", "retry", "class", "if", "unless", "while", "until", "for", "return",
                "break", "next",
            ]
            .into_iter()
            .find(|keyword| matches!(p.token(), Token::Word(w) if w == keyword));
            match keyword {
                Some(_) => p.pos += 1,
                None if p.module_ahead() => {
                    return p.err(
                        "module declarations are only supported at the top level and in module bodies",
                    );
                }
                None => (),
            }
            keyword
        };
        match keyword {
            Some("raise") => self.raise_statement().await,
            Some("retry") => {
                let p = self.p();
                if p.starts_expression()
                    && !matches!(p.token(), Token::Word(w) if matches!(w.as_str(), "if"|"unless"|"while"|"until"))
                {
                    return p.err("retry does not accept a value");
                }
                Ok(Statement::Retry)
            }
            Some("class") => Ok(Statement::UnboundClass(self.nested_class().await?.name)),
            Some(keyword @ ("if" | "unless")) => self.if_stmt(keyword == "unless").await,
            Some(keyword @ ("while" | "until")) => self.while_stmt(keyword == "until").await,
            Some("for") => self.for_stmt().await,
            Some(flow) => self.flow_statement(flow).await,
            None if self.p().token() == &Token::Op("*") || self.p().assignment_ahead()? => {
                self.assignment_statement().await
            }
            None => {
                let start = self.p().pos;
                let expr = self.line_expr(0).await?;
                // Go reads an expression followed by a comma as a destructuring target list.
                let listed = self.p().token() == &Token::P(',');
                if listed {
                    self.p().pos = start;
                    return self.assignment_statement().await;
                }
                Ok(Statement::Expr(expr))
            }
        }
    }

    async fn flow_statement(&self, flow: &str) -> Result<Statement> {
        let value = {
            let p = self.p();
            let modifier = matches!(p.token(), Token::Word(w) if matches!(w.as_str(), "if" | "unless" | "while" | "until"));
            !modifier && p.starts_expression()
        };
        let value = if value {
            let first = self.line_expr(0).await?;
            Some(if flow == "return" {
                self.return_values(first).await?
            } else {
                first
            })
        } else {
            None
        };
        Ok(match flow {
            "return" => Statement::Return(value),
            "break" => Statement::Break(value),
            _ => Statement::Next(value),
        })
    }

    async fn assignment_statement(&self) -> Result<Statement> {
        let start = self.p().pos;
        let (target, _) = self.target(true, false).await?;
        let op = {
            let mut p = self.p();
            p.lines()?;
            let op = match p.token() {
                Token::Op(op) if assignment(op) => Some(*op),
                _ => None,
            };
            // Go parses the statement's expression first and makes it a target only
            // when a comma or an assignment operator follows.
            match (op, &target) {
                (None, Target::Tuple(_)) => {
                    let last = p.previous_index()?;
                    return Err(Error::syntax(
                        p.work,
                        p.position(last),
                        "parallel assignment targets require '='",
                    ));
                }
                (None, _) => p.pos = start,
                (Some(op), Target::Tuple(_)) if op != "=" => {
                    return Err(Error::syntax(
                        p.work,
                        p.position(p.pos),
                        "compound assignment is not supported for destructuring targets",
                    ));
                }
                (Some(_), Target::Value(expr)) => {
                    if let Some(offset) = expr.safe_navigation() {
                        return Err(Error::syntax(
                            p.work,
                            offset,
                            "safe navigation cannot be used as an assignment target",
                        ));
                    }
                    if !expr.assignable() {
                        return p.unexpected(p.pos);
                    }
                }
                _ => (),
            }
            if op.is_some() {
                p.pos += 1;
                p.line_breaks()?;
            }
            op
        };
        let Some(op) = op else {
            return Ok(Statement::Expr(self.line_expr(0).await?));
        };
        let first = self.line_expr(0).await?;
        let rhs = if matches!(target, Target::Tuple(_)) && self.p().take_p(',') {
            let work = self.p().work;
            let mut items = Buffer::from_array(work, [first])?;
            loop {
                {
                    let p = self.p();
                    if p.at_end() || p.token() == &Token::EndLine {
                        break;
                    }
                }
                items.push(work, self.line_expr(0).await?)?;
                if !self.p().take_p(',') {
                    break;
                }
            }
            let depth = 1 + items.iter().map(|e| e.depth).max().unwrap_or(0);
            self.p().make(Node::Array(items), depth)?
        } else {
            first
        };
        self.p().declare_target(&target)?;
        Ok(Statement::Assign(target, op, rhs))
    }

    async fn return_values(&self, first: Expr) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        if !self.p().comma_on_line()? {
            return Ok(first);
        }
        let mut items = Buffer::from_array(work, [first])?;
        while self.p().comma_on_line()? {
            {
                let mut p = self.p();
                p.bump()?;
                p.line_breaks()?;
            }
            items.push(work, self.line_expr(0).await?)?;
        }
        let depth = 1 + items.iter().map(|e| e.depth).max().unwrap_or(0);
        self.p().make(Node::Array(items), depth)
    }

    async fn while_stmt(&self, until: bool) -> Result<Statement> {
        let previous = {
            let mut p = self.p();
            p.work.charge(1)?;
            let groups = p.groups;
            p.loop_condition.replace(groups)
        };
        let cond = self.line_expr(0).await;
        self.p().loop_condition = previous;
        let mut cond = cond?;
        {
            let mut p = self.p();
            if until {
                cond = p.negate(cond)?;
            }
            p.word("do");
        }
        let body = self.block(&["end"]).await?;
        self.p().expect_word("end")?;
        Ok(Statement::While(cond, body, None))
    }

    async fn for_stmt(&self) -> Result<Statement> {
        self.p().work.charge(1)?;
        let (target, _) = self.target(false, false).await?;
        let previous = {
            let mut p = self.p();
            if !target.is_binding() {
                let offset = target
                    .offset()
                    .map_or(p.position(p.pos), |offset| offset as usize);
                return Err(Error::syntax(p.work, offset, "invalid for loop target"));
            }
            p.expect_word("in")?;
            let groups = p.groups;
            p.loop_condition.replace(groups)
        };
        let iterable = self.line_expr(0).await;
        self.p().loop_condition = previous;
        let iterable = iterable?;
        {
            let mut p = self.p();
            p.word("do");
            p.declare_target(&target)?;
        }
        let body = self.block(&["end"]).await?;
        self.p().expect_word("end")?;
        Ok(Statement::For(target, iterable, body))
    }

    async fn target(&self, first_expression: bool, typed: bool) -> Result<(Target, bool)> {
        let work = self.p().work;
        work.charge(1)?;
        let mut parts = Buffer::new();
        let mut tuple = false;
        let mut has_rest = false;
        loop {
            let (rest, bare_rest, close) = {
                let mut p = self.p();
                let rest = p.token() == &Token::Op("*");
                if rest {
                    p.bump()?;
                    if has_rest {
                        return p.err("duplicate rest assignment target");
                    }
                    has_rest = true;
                    tuple = true;
                }
                let bare_rest = rest
                    && (matches!(p.token(), Token::P(',' | ')' | ']') | Token::Op("="))
                        || matches!(p.token(), Token::Word(w) if w=="in"));
                let grouped = !first_expression || !parts.is_empty() || rest;
                let close = if bare_rest {
                    None
                } else if grouped && p.take_p('(') {
                    Some(')')
                } else if grouped && p.take_p('[') {
                    Some(']')
                } else {
                    None
                };
                (rest, bare_rest, close)
            };
            let mut value = if bare_rest {
                None
            } else if let Some(close) = close {
                // Go counts each nested destructuring group against the syntax limit.
                {
                    let mut p = self.p();
                    p.enter()?;
                    p.lines()?;
                }
                let (inner, tuple) = self.nested_target(typed).await?;
                let mut p = self.p();
                p.lines()?;
                p.expect_p(close)?;
                p.depth -= 1;
                // Like Go, every group destructures one level; commas inside
                // it list that level's parts.
                Some(if tuple {
                    inner
                } else {
                    Target::Tuple(Buffer::from_array(work, [(Some(inner), false)])?)
                })
            } else if typed && matches!(self.p().token(), Token::Word(_)) {
                let mut p = self.p();
                let name = p.name()?;
                Some(Target::Value(p.make(Node::Var(name), 1)?))
            } else {
                let expression = self.line_expr(0).await?;
                let p = self.p();
                // Go checks a statement's lone target only once an operator follows.
                let listed = !parts.is_empty() || rest || p.token() == &Token::P(',');
                if !first_expression || listed {
                    if let Some(offset) = expression.safe_navigation() {
                        return Err(Error::syntax(
                            p.work,
                            offset,
                            "safe navigation cannot be used as an assignment target",
                        ));
                    }
                }
                if (if first_expression { listed } else { !typed }) && !expression.assignable() {
                    return Err(Error::syntax(
                        p.work,
                        expression.offset as usize,
                        "invalid destructuring assignment target",
                    ));
                }
                Some(Target::Value(expression))
            };
            let mut p = self.p();
            if typed && value.is_some() && p.take_p(':') {
                let ty = p.type_expr(1, false)?;
                if rest && !ty.captures(false) {
                    return p.err("rest target annotation must accept an array");
                }
                value = Some(Target::Typed(Boxed::new(work, value.take().unwrap())?, ty));
            }
            parts.push(work, (value, rest))?;
            if !p.take_p(',') {
                break;
            }
            tuple = true;
            p.lines()?;
        }
        let target = if tuple {
            Target::Tuple(parts)
        } else {
            parts.pop().unwrap().0.unwrap()
        };
        self.p().check_depth(target.depth())?;
        Ok((target, tuple))
    }

    async fn if_stmt(&self, unless: bool) -> Result<Statement> {
        let work = self.p().work;
        work.charge(1)?;
        let mut branches = Buffer::new();
        let alternate = loop {
            let mut cond = self.condition().await?;
            {
                let mut p = self.p();
                if unless {
                    cond = p.negate(cond)?;
                }
                p.word("then");
                p.lines()?;
            }
            let body = self.block(&["else", "elsif", "end"]).await?;
            branches.push(work, (cond, body))?;
            let alternate = {
                let mut p = self.p();
                if p.word("elsif") {
                    if unless {
                        return p.err("unless does not support elsif");
                    }
                    continue;
                }
                let alternate = p.word("else");
                if alternate {
                    p.lines()?;
                } else {
                    p.expect_word("end")?;
                }
                alternate
            };
            if !alternate {
                break Buffer::new();
            }
            let alternate = self.block(&["end"]).await?;
            self.p().expect_word("end")?;
            break alternate;
        };
        Ok(Statement::If(branches, alternate, None))
    }

    async fn if_expr(&self, unless: bool) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        let mut branches = Buffer::new();
        let alternate = loop {
            let mut cond = self.condition().await?;
            {
                let mut p = self.p();
                if unless {
                    cond = p.negate(cond)?;
                }
                p.word("then");
                p.lines()?;
            }
            let yes = self.expr(0).await?;
            branches.push(work, (cond, yes))?;
            let alternate = {
                let mut p = self.p();
                p.lines()?;
                if p.word("elsif") {
                    if unless {
                        return p.err("unless does not support elsif");
                    }
                    continue;
                }
                let alternate = p.word("else");
                if alternate {
                    p.lines()?;
                } else {
                    p.expect_word("end")?;
                }
                alternate
            };
            if !alternate {
                break self.p().make(Node::Literal(Value::nil()), 1)?;
            }
            let no = self.expr(0).await?;
            let mut p = self.p();
            p.lines()?;
            p.expect_word("end")?;
            break no;
        };
        let depth = 1 + branches
            .iter()
            .map(|(cond, yes)| cond.depth.max(yes.depth))
            .max()
            .unwrap_or(0)
            .max(alternate.depth);
        self.p().make(
            Node::Conditional(branches, Boxed::new(work, alternate)?),
            depth,
        )
    }

    async fn case_expr(&self) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        self.p().lines()?;
        let target = if matches!(self.p().token(), Token::Word(w) if w=="when") {
            None
        } else {
            Some(Boxed::new(work, self.line_expr(0).await?)?)
        };
        self.p().lines()?;
        let mut clauses = Buffer::new();
        while self.p().word("when") {
            let mut values = Buffer::new();
            loop {
                let splat = {
                    let mut p = self.p();
                    let splat = p.token() == &Token::Op("*");
                    if splat {
                        p.bump()?;
                    }
                    splat
                };
                values.push(work, (self.condition().await?, splat))?;
                let mut p = self.p();
                if !p.take_p(',') {
                    break;
                }
                p.lines()?;
            }
            {
                let mut p = self.p();
                p.word("then");
                p.lines()?;
            }
            let result = self.expr(0).await?;
            clauses.push(work, When { values, result })?;
            self.p().lines()?;
        }
        if clauses.is_empty() {
            return self.p().expected(Label::Text("when"));
        }
        let alternate = if self.p().word("else") {
            self.p().lines()?;
            Some(Boxed::new(work, self.expr(0).await?)?)
        } else {
            None
        };
        let mut p = self.p();
        p.lines()?;
        p.expect_word("end")?;
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
        p.make(Node::Case(target, clauses, alternate), depth)
    }

    async fn expression(&self, min: u8) -> Result<Expr> {
        {
            let mut p = self.p();
            p.work.charge(1)?;
            p.enter()?;
        }
        let lhs = self.prefix().await?;
        let result = self.expr_tail(lhs, min, None).await;
        self.p().depth -= 1;
        result
    }

    async fn prefix(&self) -> Result<Expr> {
        let (offset, grouped) = {
            let p = self.p();
            p.work.charge(1)?;
            (p.tokens[p.pos].offset as u32, p.token() == &Token::P('('))
        };
        let mut expr = self.prefix_node().await?;
        if !grouped {
            expr.offset = offset;
        }
        Ok(expr)
    }

    async fn prefix_node(&self) -> Result<Expr> {
        let (offset, token) = {
            let mut p = self.p();
            p.work.charge(1)?;
            let offset = p.tokens[p.pos].offset as u32;
            (offset, p.bump()?)
        };
        match token {
            Token::Word(w) => Box::pin(self.word_expression(w.as_str(), offset)).await,
            Token::P('(') => self.group_expression().await,
            Token::P('[') => self.array_expression().await,
            Token::P('{') => Box::pin(self.hash_expr()).await,
            Token::Op(op @ (".." | "...")) => Box::pin(self.open_range_expression(op)).await,
            Token::Op(op @ ("-" | "+" | "!")) => Box::pin(self.unary_prefix(op)).await,
            token => self.p().leaf(token),
        }
    }

    async fn group_expression(&self) -> Result<Expr> {
        {
            let mut p = self.p();
            p.groups += 1;
            p.lines()?;
        }
        let e = self.expr(0).await?;
        let mut p = self.p();
        p.lines()?;
        p.expect_p(')')?;
        p.groups -= 1;
        Ok(e)
    }

    async fn array_expression(&self) -> Result<Expr> {
        let a = self.arguments(']').await?;
        let d = 1 + a.iter().map(|e| e.depth).max().unwrap_or(0);
        self.p().make(Node::Array(a), d)
    }

    async fn open_range_expression(&self, op: &str) -> Result<Expr> {
        {
            let mut p = self.p();
            if p.groups > 0 {
                p.lines()?;
            }
        }
        let end = self.expr(8).await?;
        let depth = end.depth + 1;
        let p = self.p();
        p.make(
            Node::Range(None, Some(Boxed::new(p.work, end)?), op == "..."),
            depth,
        )
    }

    async fn word_expression(&self, w: &str, offset: u32) -> Result<Expr> {
        self.p().work.charge(1)?;
        match w {
            "nil" => self.p().make(Node::Literal(Value::nil()), 1),
            "true" => self.p().make(Node::Literal(Value::boolean(true)), 1),
            "false" => self.p().make(Node::Literal(Value::boolean(false)), 1),
            "if" | "unless" => self.if_expr(w == "unless").await,
            "case" => self.case_expr().await,
            "yield" => self.yield_expr().await,
            "begin" => self.begin_expression().await,
            "while" | "until" | "for" => self.loop_expression(w, offset).await,
            _ if reserved(w) && w != "then" => Err(Error::syntax(
                self.p().work,
                offset as usize,
                format_args!("unexpected token {}", Label::word(w)),
            )),
            _ => self.p().variable_name(w),
        }
    }

    async fn begin_expression(&self) -> Result<Expr> {
        let body = self.block(&["rescue", "else", "ensure", "end"]).await?;
        let attempt = self.rescue_tail(body, false).await?;
        let depth = attempt.depth();
        let p = self.p();
        p.make(Node::Try(Boxed::new(p.work, attempt)?), depth)
    }

    async fn loop_expression(&self, word: &str, offset: u32) -> Result<Expr> {
        let stmt = if word == "for" {
            self.for_stmt().await?
        } else {
            self.while_stmt(word == "until").await?
        };
        let stmt = stmt.at(offset);
        let depth = stmt.depth;
        let p = self.p();
        p.make(Node::Compound(Boxed::new(p.work, stmt)?), depth)
    }

    async fn unary_prefix(&self, op: &'static str) -> Result<Expr> {
        let literal = {
            let p = self.p();
            p.work.charge(1)?;
            p.negative_literal(op)?
        };
        let value = if literal {
            self.prefix().await?
        } else {
            self.p().line_breaks()?;
            self.expr(13).await?
        };
        let depth = value.depth + 1;
        let p = self.p();
        p.make(Node::Unary(op, Boxed::new(p.work, value)?), depth)
    }

    async fn hash_group(&self) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        let mut entries = Buffer::new();
        let closed = {
            let mut p = self.p();
            p.groups += 1;
            p.line_breaks()?;
            p.take_p('}')
        };
        if !closed {
            loop {
                let (key, shorthand) = self.p().hash_label()?;
                let value = match shorthand {
                    Some(value) => value,
                    None => self.expr(0).await?,
                };
                entries.push(work, (key, value))?;
                let mut p = self.p();
                p.line_breaks()?;
                if p.take_p('}') {
                    break;
                }
                if !matches!(p.token(), Token::P(',') | Token::Eof) {
                    return p.err(INVALID_HASH_PAIR);
                }
                p.expect_p(',')?;
                p.line_breaks()?;
                if p.take_p('}') {
                    break;
                }
            }
        }
        let d = 1 + entries.iter().map(|(_, e)| e.depth).max().unwrap_or(0);
        let mut p = self.p();
        p.groups -= 1;
        p.make(Node::Hash(entries), d)
    }

    async fn interpolated(&self) -> Result<Expr> {
        let expr = self.line_expr(0).await?;
        let p = self.p();
        if p.token() != &Token::Eof {
            return p.err("string interpolation must contain a single expression");
        }
        Ok(expr)
    }

    async fn expr_tail(&self, mut lhs: Expr, min: u8, mut next: Option<Suffix>) -> Result<Expr> {
        self.p().work.charge(1)?;
        loop {
            let suffix = match next.take() {
                Some(suffix) => Some(suffix),
                None => self.p().expression_suffix(&lhs, min)?,
            };
            let Some(suffix) = suffix else {
                return Ok(lhs);
            };
            lhs = match suffix {
                Suffix::Rescue => Box::pin(self.rescue_modifier(lhs)).await,
                Suffix::Command => self.command_expression(lhs).await,
                Suffix::Block(brace) => Box::pin(self.block_expression(lhs, brace)).await,
                Suffix::Call => {
                    let args = self.call_arguments().await?;
                    self.p().parenthesized_call(lhs, args)
                }
                Suffix::Scope => Box::pin(self.scoped_expression(lhs)).await,
                Suffix::Member(safe) => self.member_expression(lhs, safe).await,
                Suffix::Index(offset) => self.index_expression(lhs, offset).await,
                Suffix::Ternary(offset) => Box::pin(self.ternary_expression(lhs, offset)).await,
                Suffix::Binary(op, right, offset) => {
                    self.binary_expression(lhs, op, right, offset).await
                }
            }?;
        }
    }

    async fn index_expression(&self, lhs: Expr, offset: u32) -> Result<Expr> {
        let indexes = self.arguments(']').await?;
        let p = self.p();
        if indexes.is_empty() {
            return Err(Error::syntax(
                p.work,
                p.position(p.pos - 1),
                "index expression requires at least one selector",
            ));
        }
        let d = 1 + lhs
            .depth
            .max(indexes.iter().map(|e| e.depth).max().unwrap_or(0));
        p.make_at(Node::Index(Boxed::new(p.work, lhs)?, indexes), d, offset)
    }

    async fn binary_expression(
        &self,
        lhs: Expr,
        op: &'static str,
        right: u8,
        offset: u32,
    ) -> Result<Expr> {
        let work = self.p().work;
        self.p().bump()?;
        if matches!(op, ".." | "...") {
            let has_end = {
                let mut p = self.p();
                if p.groups > 0 {
                    p.lines()?;
                }
                // Go ends a range at a pending condition's `then` even inside groups.
                let stop = p.then_stop.is_some()
                    && matches!(p.token(), Token::Word(word) if word == "then");
                p.starts_expression() && !stop
            };
            let end = if has_end {
                Some(Boxed::new(work, self.expr(right).await?)?)
            } else {
                None
            };
            let depth = 1 + lhs.depth.max(end.as_ref().map_or(0, |e| e.depth));
            return self.p().make_at(
                Node::Range(Some(Boxed::new(work, lhs)?), end, op == "..."),
                depth,
                offset,
            );
        }
        self.p().line_breaks()?;
        let rhs = self.expr(right).await?;
        let depth = 1 + lhs.depth.max(rhs.depth);
        self.p().make_at(
            Node::Binary(op, Boxed::new(work, lhs)?, Boxed::new(work, rhs)?),
            depth,
            offset,
        )
    }

    async fn ternary_expression(&self, condition: Expr, offset: u32) -> Result<Expr> {
        let work = self.p().work;
        {
            let mut p = self.p();
            work.charge(1)?;
            p.lines()?;
            let groups = p.groups;
            p.ternaries.push(work, groups)?;
        }
        let yes = self.expr(0).await?;
        {
            let mut p = self.p();
            p.ternaries.pop();
            p.ternary_separator()?;
            p.expect_p(':')?;
            p.lines()?;
        }
        let no = self.expr(2).await?;
        let depth = 1 + condition.depth.max(yes.depth).max(no.depth);
        self.p().make_at(
            Node::Conditional(
                Buffer::from_array(work, [(condition, yes)])?,
                Boxed::new(work, no)?,
            ),
            depth,
            offset,
        )
    }

    async fn command_expression(&self, lhs: Expr) -> Result<Expr> {
        let group = {
            let mut p = self.p();
            p.work.charge(1)?;
            p.command_depth += 1;
            if p.command_depth > 64 {
                return p.err("parenless call nesting too deep");
            }
            let groups = p.groups;
            let group = std::mem::replace(&mut p.command_group, groups);
            if p.token() == &Token::Op("/") {
                p.expand_regex()?;
            }
            group
        };
        let args = self.command_arguments().await?;
        let mut p = self.p();
        p.command_group = group;
        p.command_depth -= 1;
        let depth = 1 + call_depth(&lhs).max(args.iter().map(|a| a.value.depth).max().unwrap_or(0));
        let offset = lhs.offset;
        let node = match lhs.into_node() {
            Node::Var(name) => Node::Call(name, args, CallForm::Bare),
            Node::Member(receiver, name) => Node::Method(receiver, name, args, CallForm::Bare),
            Node::SafeMember(receiver, name) => {
                Node::SafeMethod(receiver, name, args, CallForm::Bare)
            }
            _ => unreachable!(),
        };
        p.make_at(node, depth, offset)
    }

    async fn block_expression(&self, mut lhs: Expr, brace: bool) -> Result<Expr> {
        self.p().work.charge(1)?;
        let offset = lhs.offset;
        let block = self.attached_block(brace).await?;
        if matches!(lhs.node, Node::BlockCall(..)) {
            let Node::BlockCall(call, _) = lhs.into_node() else {
                unreachable!()
            };
            lhs = call.into_inner();
        }
        // Go counts the block literal as a node below its call.
        let params = block
            .params
            .iter()
            .map(|target| target.depth())
            .max()
            .unwrap_or(0);
        let body = block.body.iter().map(|s| s.depth).max().unwrap_or(0);
        let depth = 1 + lhs.depth.max(1 + body.max(params));
        let p = self.p();
        p.make_at(
            Node::BlockCall(Boxed::new(p.work, lhs)?, block),
            depth,
            offset,
        )
    }

    async fn scoped_expression(&self, lhs: Expr) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        let offset = lhs.offset;
        let (name, parenthesized) = {
            let mut p = self.p();
            p.bump()?;
            p.line_breaks()?;
            let name_offset = p.tokens[p.pos].offset;
            let Token::Word(name) = p.bump()? else {
                return Err(Error::syntax(
                    work,
                    name_offset,
                    "expected scoped member name",
                ));
            };
            if name.starts_with('@') || (keyword(&name) && name != "enum") {
                return Err(Error::syntax(
                    work,
                    name_offset,
                    "expected scoped member name",
                ));
            }
            (Name::new(work, &name)?, p.take_p('('))
        };
        let args = if parenthesized {
            Some(self.call_arguments().await?)
        } else {
            None
        };
        let arguments = args.as_ref().map_or(0, |args| {
            args.iter().map(|arg| arg.value.depth).max().unwrap_or(0)
        });
        let receiver = if args.is_some() {
            1 + lhs.depth
        } else {
            lhs.depth
        };
        let depth = 1 + receiver.max(arguments);
        self.p().make_at(
            Node::Scope(Boxed::new(work, lhs)?, name, args),
            depth,
            offset,
        )
    }

    async fn member_expression(&self, lhs: Expr, safe: bool) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        let offset = lhs.offset;
        let (name, parenthesized) = {
            let mut p = self.p();
            p.bump()?;
            p.line_breaks()?;
            let name = p.member_name()?;
            p.note(|record| record.member(&name, &lhs));
            (name, p.take_p('('))
        };
        if parenthesized {
            let args = self.call_arguments().await?;
            // Go nests the member access below the call.
            let depth =
                1 + (1 + lhs.depth).max(args.iter().map(|a| a.value.depth).max().unwrap_or(0));
            let method = if safe { Node::SafeMethod } else { Node::Method };
            self.p().make_at(
                method(Boxed::new(work, lhs)?, name, args, CallForm::Parenthesized),
                depth,
                offset,
            )
        } else {
            let depth = lhs.depth + 1;
            let member = if safe { Node::SafeMember } else { Node::Member };
            self.p()
                .make_at(member(Boxed::new(work, lhs)?, name), depth, offset)
        }
    }

    async fn attached_block(&self, brace: bool) -> Result<Block> {
        let (offset, outer, outer_it) = {
            let mut p = self.p();
            p.work.charge(1)?;
            let offset = p.tokens[p.pos].offset as u32;
            p.bump()?;
            p.lines()?;
            (offset, p.locals.copy(p.work)?, p.declared_it)
        };
        let infer_it = !outer_it;
        let (params, explicit) = self.block_parameters().await?;
        let (previous_loop, previous_then, command_depth) = {
            let mut p = self.p();
            (
                p.loop_condition.take(),
                p.then_stop.take(),
                std::mem::replace(&mut p.command_depth, 0),
            )
        };
        let body = self.block(if brace { &["}"] } else { &["end"] }).await?;
        let mut p = self.p();
        p.command_depth = command_depth;
        p.then_stop = previous_then;
        p.loop_condition = previous_loop;
        if brace {
            p.expect_p('}')?;
        } else {
            p.expect_word("end")?;
        }
        p.locals = outer;
        p.declared_it = outer_it;
        Ok(Block {
            offset,
            params,
            body,
            implicit: !explicit,
            infer_it,
        })
    }

    async fn block_parameters(&self) -> Result<(Buffer<Target>, bool)> {
        let work = self.p().work;
        let mut params = Buffer::new();
        let explicit = if self.p().token() == &Token::Op("||") {
            self.p().bump()?;
            true
        } else if self.p().take_p('|') {
            self.p().lines()?;
            if !self.p().take_p('|') {
                loop {
                    let close = {
                        let mut p = self.p();
                        if p.take_p('(') {
                            Some(')')
                        } else if p.take_p('[') {
                            Some(']')
                        } else {
                            None
                        }
                    };
                    let target = if let Some(close) = close {
                        let (target, tuple) = self.nested_target(true).await?;
                        let mut p = self.p();
                        p.lines()?;
                        p.expect_p(close)?;
                        if tuple {
                            target
                        } else {
                            Target::Tuple(Buffer::from_array(work, [(Some(target), false)])?)
                        }
                    } else {
                        let mut p = self.p();
                        let name = p.name()?;
                        let target = Target::Value(p.make(Node::Var(name), 1)?);
                        if p.take_p(':') {
                            Target::Typed(Boxed::new(work, target)?, p.type_expr(0, true)?)
                        } else {
                            target
                        }
                    };
                    let mut p = self.p();
                    if !target.is_binding() {
                        return p.err("invalid block parameter");
                    }
                    p.declare_target(&target)?;
                    params.push(work, target)?;
                    p.lines()?;
                    if p.take_p('|') {
                        break;
                    }
                    p.expect_p(',')?;
                    p.lines()?;
                }
            }
            true
        } else {
            false
        };
        if !explicit {
            let mut p = self.p();
            p.locals.insert(work, Name::new(work, "it")?, ())?;
            for n in ["_1", "_2", "_3", "_4", "_5", "_6", "_7", "_8", "_9"] {
                p.locals.insert(work, Name::new(work, n)?, ())?;
            }
        }
        Ok((params, explicit))
    }

    async fn yield_expr(&self) -> Result<Expr> {
        let work = self.p().work;
        let (line, parenthesized) = {
            let mut p = self.p();
            work.charge(1)?;
            let line = p.previous()?.line;
            let mut next = p.pos;
            while p.tokens[next].token == Token::EndLine
                && p.tokens[next].line != p.tokens[next].end_line
            {
                work.charge(1)?;
                next += 1;
            }
            if p.tokens[next].token == Token::P('(') {
                p.pos = next;
            }
            (line, p.take_p('('))
        };
        let args = if parenthesized {
            self.arguments(')').await?
        } else {
            let mut args = Buffer::new();
            let starts = {
                let p = self.p();
                p.tokens[p.pos].line == line && p.starts_expression()
            };
            if starts {
                args.push(work, self.line_expr(0).await?)?;
                loop {
                    {
                        let mut p = self.p();
                        if !(p.token() == &Token::P(',')
                            && p.tokens[p.pos].line == line
                            && p.tokens[p.pos + 1].line == line)
                        {
                            break;
                        }
                        p.bump()?;
                    }
                    args.push(work, self.line_expr(0).await?)?;
                }
            }
            args
        };
        let depth = 1 + args.iter().map(|arg| arg.depth).max().unwrap_or(0);
        self.p().make(Node::Yield(args), depth)
    }

    async fn command_arguments(&self) -> Result<Buffer<Argument>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut args = Buffer::new();
        let mut keywords = false;
        loop {
            let argument = self.call_argument(false).await?;
            let keyword = matches!(
                argument.kind,
                ArgumentKind::Keyword(_) | ArgumentKind::KeywordSplat
            );
            let mut p = self.p();
            if keywords && !keyword {
                return p.err("positional arguments cannot follow keywords");
            }
            keywords |= keyword;
            args.push(work, argument)?;
            let last = p.previous()?;
            if p.token() != &Token::P(',')
                || p.tokens[p.pos].line != last.line
                || p.tokens[p.pos + 1].line != last.line
                || !p.command_argument_start(p.pos + 1, true)
            {
                break;
            }
            p.bump()?;
        }
        Ok(args)
    }

    async fn arguments(&self, close: char) -> Result<Buffer<Expr>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut args = Buffer::new();
        {
            let mut p = self.p();
            p.groups += 1;
            p.lines()?;
            if p.take_p(close) {
                p.groups -= 1;
                return Ok(args);
            }
        }
        loop {
            args.push(work, self.expr(0).await?)?;
            let mut p = self.p();
            p.lines()?;
            if p.take_p(close) {
                break;
            }
            if p.token() != &Token::P(',') {
                return p.expected(Label::Char(close));
            }
            p.expect_p(',')?;
            p.lines()?;
            if p.take_p(close) {
                break;
            }
        }
        self.p().groups -= 1;
        Ok(args)
    }

    async fn call_arguments(&self) -> Result<Buffer<Argument>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut args = Buffer::new();
        let mut keywords = false;
        {
            let mut p = self.p();
            p.groups += 1;
            p.line_breaks()?;
            if p.take_p(')') {
                p.groups -= 1;
                return Ok(args);
            }
        }
        loop {
            let argument = self.call_argument(true).await?;
            let keyword = matches!(
                argument.kind,
                ArgumentKind::Keyword(_) | ArgumentKind::KeywordSplat
            );
            let mut p = self.p();
            if keywords && !keyword {
                return p.err("positional arguments cannot follow keywords");
            }
            keywords |= keyword;
            args.push(work, argument)?;
            p.line_breaks()?;
            if p.take_p(')') {
                break;
            }
            if p.token() != &Token::P(',') {
                return p.expected(Label::Char(')'));
            }
            p.expect_p(',')?;
            p.line_breaks()?;
            if p.take_p(')') {
                break;
            }
        }
        self.p().groups -= 1;
        Ok(args)
    }

    async fn call_argument(&self, parenthesized: bool) -> Result<Argument> {
        let ampersand = {
            let mut p = self.p();
            let offset = p.tokens[p.pos].offset;
            (p.token() == &Token::Op("&")).then(|| {
                p.pos += 1;
                offset
            })
        };
        if let Some(offset) = ampersand {
            // Go parses the operand before refusing the removed block argument.
            self.expr(0).await?;
            return Err(Error::syntax(
                self.p().work,
                offset,
                "block arguments are not supported; a block is not a value. Write the block at \
                 the call that runs it, as in `words.map { |word| word.upcase }`",
            ));
        }
        let (kind, literal) = {
            let mut p = self.p();
            p.work.charge(1)?;
            let kind = p.argument_kind()?;
            if parenthesized || matches!(kind, ArgumentKind::Splat | ArgumentKind::KeywordSplat) {
                p.line_breaks()?;
            }
            let literal = p.literal_argument(&kind, parenthesized)?;
            (kind, literal)
        };
        let value = match literal {
            Some(value) => value,
            None => self.expr(0).await?,
        };
        Ok(Argument { kind, value })
    }
}

impl<'a> Parser<'a> {
    fn parameter_name(&mut self) -> Result<(Name, bool, ParamKind)> {
        let kind = match self.token() {
            Token::Op("*") => {
                self.bump()?;
                ParamKind::Rest
            }
            Token::Op("**") => {
                self.bump()?;
                ParamKind::KeywordRest
            }
            _ => ParamKind::Positional,
        };
        let name = self.name()?;
        if name.starts_with("@@") {
            return self.err("expected parameter name");
        }
        let instance = name.starts_with('@');
        if instance && kind != ParamKind::Positional {
            return self.err("capture parameters must use local names");
        }
        let name = if let Some(name) = name.strip_prefix('@') {
            Name::new(self.work, name)?
        } else {
            name
        };
        Ok((name, instance, kind))
    }
    fn token(&self) -> &Token<'a> {
        &self.tokens[self.pos].token
    }
    /// Adds a tooling fact when this parse is recording.
    fn note(&mut self, add: impl FnOnce(&mut record::Record)) {
        if let Some(record) = self.record.as_deref_mut() {
            add(record);
        }
    }
    fn err<T>(&self, message: impl std::fmt::Display) -> Result<T> {
        self.work.charge(1)?;
        Err(Error::syntax(self.work, self.position(self.pos), message))
    }
    /// The source offset at which Go reports the token at `index`. Its lexer
    /// stamps a multi-character operator at the operator's last character,
    /// except `<=>` and `===`.
    fn position(&self, index: usize) -> usize {
        let lexeme = &self.tokens[index];
        match lexeme.token {
            Token::Op(op) if !matches!(op, "<=>" | "===") => lexeme.offset + op.len() - 1,
            _ => lexeme.offset,
        }
    }
    /// The index of the token Go's parser sees at `index`, which has no
    /// token for a line break.
    fn significant(&self, mut index: usize) -> usize {
        while index + 1 < self.tokens.len()
            && self.tokens[index].token == Token::EndLine
            && self.source.as_bytes().get(self.tokens[index].offset) != Some(&b';')
        {
            index += 1;
        }
        index
    }
    /// Go's diagnostic name for the token at `index`.
    fn label(&self, index: usize) -> Label<'a> {
        let lexeme = &self.tokens[index];
        match &lexeme.token {
            Token::Word(word) if word.starts_with("@@") => Label::Text("class variable"),
            Token::Word(word) if word.starts_with('@') => Label::Text("instance variable"),
            Token::Word(word) => Label::word(word.as_str()),
            Token::Int(_) | Token::BigInt(..) => Label::Text("integer"),
            Token::Float(_) => Label::Text("float"),
            Token::Bytes(_) | Token::Template(_) => Label::Text("string"),
            Token::Regex(..) => Label::Quoted("regex"),
            Token::Words(words) => {
                let interpolated = words
                    .entries
                    .iter()
                    .flat_map(|entry| entry.iter())
                    .any(|part| matches!(part, Part::Expr(..)));
                Label::Text(match (interpolated, words.symbol) {
                    (false, false) => "percent word array",
                    (false, true) => "percent symbol array",
                    (true, false) => "percent interpolated word array",
                    (true, true) => "percent interpolated symbol array",
                })
            }
            Token::Invalid(_) => Label::Text("invalid token"),
            Token::P(':')
                if self.tokens.get(index + 1).is_some_and(|next| {
                    next.offset == lexeme.end
                        && matches!(
                            next.token,
                            Token::Word(_) | Token::Bytes(_) | Token::Template(_)
                        )
                }) =>
            {
                Label::Text("symbol")
            }
            Token::P(c) => Label::Char(*c),
            Token::Op(op) => Label::Quoted(op),
            Token::EndLine if self.source.as_bytes().get(lexeme.offset) == Some(&b';') => {
                Label::Char(';')
            }
            Token::EndLine | Token::Eof => Label::Text("end of input"),
        }
    }
    /// A lexer diagnostic that Go reports in place of any expectation at `index`.
    fn diagnostic(&self, index: usize) -> Option<Error> {
        match &self.tokens[index].token {
            Token::Invalid(error) if error.1.as_str() != UNSUPPORTED_CHARACTER => {
                Some(Error::syntax(self.work, error.0, error.1.as_str()))
            }
            _ => None,
        }
    }
    /// Reports Go's failed expectation of `expected` at the current token.
    fn expected<T>(&self, expected: Label<'_>) -> Result<T> {
        self.work.charge(1)?;
        let index = self.significant(self.pos);
        if let Some(error) = self.diagnostic(index) {
            return Err(error);
        }
        Err(Error::syntax(
            self.work,
            self.position(index),
            format_args!("expected {expected}, got {}", self.label(index)),
        ))
    }
    /// Reports Go's refusal of a token that cannot start an expression.
    fn unexpected<T>(&self, index: usize) -> Result<T> {
        self.work.charge(1)?;
        let index = self.significant(index);
        if let Some(error) = self.diagnostic(index) {
            return Err(error);
        }
        Err(Error::syntax(
            self.work,
            self.position(index),
            format_args!("unexpected token {}", self.label(index)),
        ))
    }
    fn bump(&mut self) -> Result<Token<'a>> {
        self.work
            .bytes(self.tokens[self.pos].end - self.tokens[self.pos].offset)?;
        let t = self.token().copy(self.work)?;
        if !matches!(t, Token::Eof) {
            self.pos += 1;
        }
        Ok(t)
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
        self.work.charge(1)?;
        if self.word(w) {
            Ok(())
        } else if w == "end" {
            // Go checks a block's closing `end` by name rather than by token.
            self.expected(Label::Text("end"))
        } else {
            self.expected(Label::word(w))
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
        self.work.charge(1)?;
        if self.take_p(c) {
            Ok(())
        } else {
            self.expected(Label::Char(c))
        }
    }
    /// Returns the end offset of the last token a declaration beginning at
    /// token `first` consumed, excluding line breaks and separators it ended on.
    fn declaration_end(&self, first: usize) -> usize {
        let mut last = self.pos.max(first + 1);
        while last > first + 1 && matches!(self.tokens[last - 1].token, Token::EndLine) {
            last -= 1;
        }
        self.tokens[last - 1].end
    }
    fn lines(&mut self) -> Result<()> {
        while matches!(self.token(), Token::EndLine) {
            self.work.charge(1)?;
            self.pos += 1;
        }
        Ok(())
    }
    fn line_breaks(&mut self) -> Result<()> {
        while self.token() == &Token::EndLine
            && self.tokens[self.pos].line != self.tokens[self.pos].end_line
        {
            self.work.charge(1)?;
            self.pos += 1;
        }
        Ok(())
    }
    fn name(&mut self) -> Result<Name> {
        self.work.charge(1)?;
        let offset = self.tokens[self.pos].offset;
        if let Token::Word(w) = self.bump()? {
            if reserved(&w) {
                return Err(Error::syntax(self.work, offset, "reserved name"));
            }
            Name::new(self.work, &w)
        } else {
            Err(Error::syntax(self.work, offset, "expected name"))
        }
    }
    fn enum_name(&mut self) -> Result<Name> {
        self.work.charge(1)?;
        let offset = self.tokens[self.pos].offset;
        match self.bump()? {
            Token::Word(name) if !keyword(&name) && !name.starts_with('@') => {
                Name::new(self.work, &name)
            }
            _ => Err(Error::syntax(self.work, offset, "expected enum identifier")),
        }
    }
    fn at_end(&self) -> bool {
        matches!(self.token(), Token::Eof)
            || matches!(self.token(),Token::Word(s) if matches!(s.as_str(),"end"|"else"|"elsif"|"when"|"rescue"|"ensure"))
    }
    fn enter(&mut self) -> Result<()> {
        self.work.charge(1)?;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            self.err(TOO_DEEP)
        } else {
            Ok(())
        }
    }
    fn check_depth(&self, depth: u32) -> Result<()> {
        if depth as usize > MAX_DEPTH {
            self.err(TOO_DEEP)
        } else {
            Ok(())
        }
    }
    fn declare_target(&mut self, target: &Target) -> Result<()> {
        self.work.charge(1)?;
        let mut names = Buffer::new();
        target.parts(|part, _| {
            if let Target::Value(Expr {
                node: Node::Var(name),
                ..
            }) = part
            {
                names.push(self.work, name.clone()).is_ok()
            } else {
                true
            }
        });
        for name in names {
            self.work.charge(1)?;
            self.declared_it |= name == "it";
            self.locals.insert(self.work, name, ())?;
        }
        Ok(())
    }
    // Like Go, the separator may start a later line, but a line-leading
    // `:name` is a symbol rather than the separator.
    fn ternary_separator(&mut self) -> Result<()> {
        let mut next = self.pos;
        while self.tokens[next].token == Token::EndLine
            && self.tokens[next].line != self.tokens[next].end_line
        {
            self.work.charge(1)?;
            next += 1;
        }
        if self.tokens[next].token == Token::P(':') && !self.symbol_start(next) {
            self.pos = next;
        }
        Ok(())
    }
    fn comma_on_line(&self) -> Result<bool> {
        Ok(self.token() == &Token::P(',') && self.tokens[self.pos].line == self.previous()?.line)
    }
    fn assignment_ahead(&self) -> Result<bool> {
        self.work.charge(1)?;
        let mut nesting = 0usize;
        let mut comma = false;
        let mut after_member_separator = false;
        for (i, lexeme) in self.tokens.from(self.pos).enumerate() {
            self.work.charge(1)?;
            match &lexeme.token {
                Token::P('(' | '[' | '{') => nesting += 1,
                Token::P(')' | ']' | '}') => {
                    if nesting == 0 {
                        return Ok(false);
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
                Token::Op(op) if nesting == 0 && assignment(op) => return Ok(true),
                Token::EndLine if nesting == 0 => {
                    if lexeme.line == lexeme.end_line {
                        return Ok(false);
                    }
                    if after_member_separator {
                        continue;
                    }
                    let next =
                        self.tokens
                            .find(self.pos + i + 1..self.tokens.len(), self.work, |l| {
                                !matches!(l.token, Token::EndLine)
                            })?;
                    if !comma
                        && !next.is_some_and(|l| {
                            matches!(l.token, Token::P('.') | Token::Op("&."))
                                || matches!(l.token, Token::Op(op) if assignment(op))
                        })
                    {
                        return Ok(false);
                    }
                }
                Token::Word(w)
                    if nesting == 0 && reserved(w) && w != "then" && !after_member_separator =>
                {
                    return Ok(false);
                }
                Token::Eof => return Ok(false),
                _ => (),
            }
            if !matches!(lexeme.token, Token::EndLine) {
                comma = lexeme.token == Token::P(',');
                after_member_separator = matches!(lexeme.token, Token::P('.') | Token::Op("&."));
            }
        }
        Ok(false)
    }
    fn negate(&self, expr: Expr) -> Result<Expr> {
        self.work.charge(1)?;
        let depth = expr.depth + 1;
        let offset = expr.offset;
        self.make_at(
            Node::Unary("!", Boxed::new(self.work, expr)?),
            depth,
            offset,
        )
    }
    fn make(&self, node: Node, depth: u32) -> Result<Expr> {
        self.work.charge(1)?;
        self.check_depth(depth)?;
        Ok(Expr {
            node,
            depth,
            offset: self.tokens[self.pos.saturating_sub(1)].offset as u32,
        })
    }
    fn make_at(&self, node: Node, depth: u32, offset: u32) -> Result<Expr> {
        self.work.charge(1)?;
        let mut expr = self.make(node, depth)?;
        expr.offset = offset;
        Ok(expr)
    }
    // Most expressions start with a single-token operand. Parse it and look
    // for a suffix here, so that only nested syntax costs a task.
    fn leaf_expression(&mut self, min: u8) -> Result<Leaf> {
        let leaf = match self.token() {
            Token::Int(_)
            | Token::BigInt(..)
            | Token::Float(_)
            | Token::Regex(..)
            | Token::Bytes(_)
            | Token::Template(_)
            | Token::Words(..)
            | Token::P(':') => true,
            Token::Word(w) => !reserved(w) || w == "then",
            _ => false,
        };
        if !leaf {
            return Ok(Leaf::Nested);
        }
        self.work.charge(1)?;
        self.enter()?;
        self.work.charge(1)?;
        let offset = self.tokens[self.pos].offset as u32;
        let mut lhs = match self.bump()? {
            Token::Word(w) => match w.as_str() {
                "nil" => self.make(Node::Literal(Value::nil()), 1)?,
                "true" => self.make(Node::Literal(Value::boolean(true)), 1)?,
                "false" => self.make(Node::Literal(Value::boolean(false)), 1)?,
                name => self.variable_name(name)?,
            },
            token => self.leaf(token)?,
        };
        lhs.offset = offset;
        Ok(match self.expression_suffix(&lhs, min)? {
            Some(suffix) => Leaf::Tail(lhs, suffix),
            None => {
                self.depth -= 1;
                Leaf::Done(lhs)
            }
        })
    }
    // Build expressions that need no nested parsing from their first token.
    fn leaf(&mut self, token: Token<'a>) -> Result<Expr> {
        match token {
            Token::Int(n) => self.make(Node::Integer(n), 1),
            Token::BigInt(text, radix) => self.make(Node::BigInteger(text, radix), 1),
            Token::Float(n) => self.make(Node::Literal(Value::float(n)), 1),
            Token::Regex(pattern, flags) => self.make(Node::Regex(pattern, flags), 1),
            Token::Bytes(b) => self.make(Node::Literal(b.into_value(false)), 1),
            Token::Template(parts) => self.template(parts, false),
            Token::Words(words) => self.words(words.into_inner()),
            Token::Invalid(error) if error.1.as_str() == UNSUPPORTED_CHARACTER => {
                self.unexpected(self.pos - 1)
            }
            Token::Invalid(error) => Err(Error::syntax(self.work, error.0, error.1.as_str())),
            Token::P(':') => self.symbol(),
            Token::Op("->") => Err(Error::syntax(
                self.work,
                self.position(self.pos - 1),
                "lambda literals are not supported; executable code is not a value. Define a \
                 named function and call it, or attach a block to the call that runs it, as in \
                 `people.map { |person| person.name }`",
            )),
            Token::Eof => self.unexpected(self.pos),
            _ => self.unexpected(self.pos - 1),
        }
    }
    fn variable_name(&self, name: &str) -> Result<Expr> {
        self.make(Node::Var(Name::new(self.work, name)?), 1)
    }
    fn negative_literal(&self, op: &str) -> Result<bool> {
        // An adjacent minus belongs to the numeric receiver; power keeps the
        // outer sign.
        Ok(op == "-"
            && self.tokens[self.pos - 1].end == self.tokens[self.pos].offset
            && matches!(
                self.token(),
                Token::Int(_) | Token::BigInt(..) | Token::Float(_)
            )
            && !self
                .tokens
                .find(self.pos + 1..self.tokens.len(), self.work, |next| {
                    next.token != Token::EndLine || next.line == next.end_line
                })?
                .is_some_and(|next| next.token == Token::Op("**")))
    }

    fn hash_label(&mut self) -> Result<(Bytes, Option<Expr>)> {
        let offset = self.tokens[self.pos].offset as u32;
        let labeled = matches!(self.token(), Token::Word(w) if !w.starts_with('@'))
            || matches!(self.token(), Token::Bytes(_));
        let colon = self.significant(self.pos + 1);
        if !labeled || self.tokens[colon].token != Token::P(':') {
            return self.err(INVALID_HASH_PAIR);
        }
        let (key, label) = match self.bump()? {
            Token::Word(w) => (Bytes::from_slice(self.work, w.as_bytes())?, Some(w)),
            Token::Bytes(b) => (b, None),
            _ => unreachable!(),
        };
        self.line_breaks()?;
        self.expect_p(':')?;
        self.line_breaks()?;
        let shorthand = if matches!(self.token(), Token::P(',' | '}') | Token::Eof) {
            let Some(name) = label else {
                let name = String::from_utf8_lossy(key.as_ref());
                let mut end = name.len().min(64);
                while !name.is_char_boundary(end) {
                    end -= 1;
                }
                let ellipsis = if end < name.len() { "..." } else { "" };
                return self.err(format_args!(
                    "missing value for hash key {}{ellipsis}",
                    &name[..end]
                ));
            };
            Some(self.make_at(Node::Var(Name::new(self.work, &name)?), 1, offset)?)
        } else {
            None
        };
        Ok((key, shorthand))
    }

    fn words(&mut self, words: lexer::Words<'a>) -> Result<Expr> {
        self.work.charge(1)?;
        let mut values = Buffer::with_capacity(self.work, words.entries.len())?;
        for word in words.entries {
            values.push(self.work, self.template(word, words.symbol)?)?;
        }
        let depth = 1 + values.iter().map(|v| v.depth).max().unwrap_or(0);
        self.make(Node::Array(values), depth)
    }

    fn template(
        &mut self,
        parts: crate::compilation::Buffer<Part<'a>>,
        symbol: bool,
    ) -> Result<Expr> {
        self.work.charge(1)?;
        if !parts.iter().any(|part| matches!(part, Part::Expr(..))) {
            let bytes = lexer::plain(parts, self.work)?;
            let value = bytes.into_value(symbol);
            return self.make(Node::Literal(value), 1);
        }
        let mut values = Buffer::with_capacity(self.work, parts.len())?;
        for part in parts {
            values.push(
                self.work,
                match part {
                    Part::Text(bytes) => self.make(Node::Literal(bytes.into_value(false)), 1)?,
                    Part::Expr(tokens, span) => {
                        self.interpolations.push(self.work, span)?;
                        self.interpolation(tokens)?
                    }
                },
            )?;
        }
        let depth = 1 + values.iter().map(|v| v.depth).max().unwrap_or(0);
        self.make(Node::Template(values, symbol), depth)
    }

    fn interpolation(
        &mut self,
        mut tokens: crate::compilation::Buffer<Lexeme<'a>>,
    ) -> Result<Expr> {
        self.work.charge(1)?;
        while tokens.len() >= 2 {
            self.work.charge(1)?;
            let tail = &tokens[tokens.len() - 2];
            if tail.token != Token::EndLine || tail.line == tail.end_line {
                break;
            }
            tokens.remove(tokens.len() - 2);
        }
        let mut parser = Parser {
            work: self.work,
            source: self.source,
            lex_depth: self.lex_depth + 1,
            tokens: Tokens::new(tokens, self.work)?,
            pos: 0,
            depth: self.depth,
            groups: 0,
            line_exprs: 0,
            command_depth: 0,
            ternaries: Buffer::new(),
            command_group: 0,
            loop_condition: None,
            then_stop: None,
            locals: std::mem::take(&mut self.locals),
            declared_it: self.declared_it,
            type_structural_error: false,
            interpolations: Buffer::new(),
            // Go parses interpolations without the member probe.
            record: None,
        };
        while parser.token() == &Token::EndLine
            && parser.tokens[parser.pos].line != parser.tokens[parser.pos].end_line
        {
            self.work.charge(1)?;
            parser.pos += 1;
        }
        // Interpolation depth is bounded by the lexer, so each level can run
        // its own task stack.
        let parsing = Parsing::new(parser);
        let result = parsing.run(Call::Interpolation);
        let parser = parsing.parser.into_inner();
        self.locals = parser.locals;
        self.declared_it = parser.declared_it;
        self.interpolations
            .extend(self.work, parser.interpolations)?;
        match result? {
            Parsed::Expr(expr) => Ok(expr),
            _ => unreachable!(),
        }
    }

    fn expand_modulo(&mut self) -> Result<()> {
        self.work.charge(1)?;
        // A quoted index can extend past a tentative percent-literal delimiter.
        // Re-lex its suffix through the next intact token boundary.
        let limit = self.tokens.last().unwrap().offset;
        let tokens = lexer::modulo(
            self.source,
            &self.tokens[self.pos],
            limit,
            self.lex_depth,
            self.work,
        )?;
        self.replace_lexed(tokens, limit)
    }

    fn expand_regex(&mut self) -> Result<()> {
        self.work.charge(1)?;
        let limit = self.tokens.last().unwrap().offset;
        let tokens = lexer::regex(
            self.source,
            &self.tokens[self.pos],
            limit,
            self.lex_depth,
            self.work,
        )?;
        self.replace_lexed(tokens, limit)
    }

    fn replace_lexed(
        &mut self,
        mut tokens: crate::compilation::Buffer<Lexeme<'a>>,
        limit: usize,
    ) -> Result<()> {
        self.work.charge(1)?;
        let mut cursor = tokens.pop().unwrap();
        let mut finish = self.pos + 1;
        loop {
            while self.tokens[finish].offset < cursor.offset {
                self.work.charge(1)?;
                finish += 1;
            }
            let end = self.tokens[finish - 1].end.max(self.tokens[finish].offset);
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
                self.work,
            )?;
            cursor = suffix.pop().unwrap();
            tokens.extend(self.work, suffix)?;
        }
        self.tokens.replace(self.pos..finish, tokens, self.work)?;
        Ok(())
    }

    fn symbol(&mut self) -> Result<Expr> {
        self.work.charge(1)?;
        let colon = self.pos - 1;
        if !self.symbol_start(colon) {
            return self.unexpected(colon);
        }
        let bytes = match self.bump()? {
            Token::Word(w) => Bytes::from_slice(self.work, w.as_bytes())?,
            Token::Bytes(b) => b,
            Token::Op(op) => Bytes::from_slice(self.work, op.as_bytes())?,
            Token::P('[') => {
                self.expect_p(']')?;
                if self.token() == &Token::Op("=")
                    && self.previous()?.end == self.tokens[self.pos].offset
                {
                    self.bump()?;
                    Bytes::from_slice(self.work, b"[]=")?
                } else {
                    Bytes::from_slice(self.work, b"[]")?
                }
            }
            _ => return self.unexpected(colon),
        };
        self.make(Node::Literal(bytes.into_value(true)), 1)
    }
    fn parenthesized_call(&mut self, lhs: Expr, args: Buffer<Argument>) -> Result<Expr> {
        self.work.charge(1)?;
        let origin = lhs.offset;
        let argument_depth = args.iter().map(|a| a.value.depth).max().unwrap_or(0);
        let d = 1 + call_depth(&lhs).max(argument_depth);
        if !matches!(
            lhs.node,
            Node::Var(_) | Node::Member(..) | Node::SafeMember(..) | Node::Scope(_, _, None)
        ) {
            let node = Node::ComputedCall(Boxed::new(self.work, lhs)?, args);
            return self.make_at(node, d, origin);
        }
        let node = match lhs.into_node() {
            Node::Var(name) => Node::Call(name, args, CallForm::Parenthesized),
            Node::Member(receiver, name) => {
                Node::Method(receiver, name, args, CallForm::Parenthesized)
            }
            Node::SafeMember(receiver, name) => {
                Node::SafeMethod(receiver, name, args, CallForm::Parenthesized)
            }
            Node::Scope(receiver, name, None) => Node::Scope(receiver, name, Some(args)),
            _ => unreachable!(),
        };
        self.make_at(node, d, origin)
    }
    fn expression_suffix(&mut self, lhs: &Expr, min: u8) -> Result<Option<Suffix>> {
        if let Some(next) = self.continuation_position(min)? {
            self.pos = next;
        }
        if min == 0
            && (self.command_depth == 0 || self.groups > self.command_group)
            && self.tokens[self.pos].line == self.previous()?.end_line
            && matches!(self.token(), Token::Word(w) if w == "rescue")
        {
            // `rescue:` labels a parenless call's keyword argument, as in Go.
            if !self.keyword_label(self.pos) {
                self.pos += 1;
                return Ok(Some(Suffix::Rescue));
            }
            if !matches!(
                lhs.node,
                Node::Var(_) | Node::Member(..) | Node::SafeMember(..)
            ) {
                return self.err("rescue modifier requires fallback expression");
            }
        }
        let offset = self.tokens[self.pos].offset as u32;
        if self.command_start(lhs, min)? {
            return Ok(Some(Suffix::Command));
        }
        let brace = self.token() == &Token::P('{');
        let do_block = matches!(self.token(), Token::Word(w) if w == "do") && self.can_attach_do();
        if (brace || do_block)
            && (do_block || self.tokens[self.pos].line == self.previous()?.end_line)
        {
            return Ok(Some(Suffix::Block(brace)));
        }
        if self.take_p('(') {
            return Ok(Some(Suffix::Call));
        }
        if self.token() == &Token::Op("::") {
            return Ok(Some(Suffix::Scope));
        }
        let safe = self.token() == &Token::Op("&.");
        if safe || self.token() == &Token::P('.') {
            return Ok(Some(Suffix::Member(safe)));
        }
        if self.take_p('[') {
            return Ok(Some(Suffix::Index(offset)));
        }
        if min <= 2 && self.take_p('?') {
            return Ok(Some(Suffix::Ternary(offset)));
        }
        if matches!(self.token(), Token::Words(words) if words.ambiguous) {
            self.expand_modulo()?;
        }
        let Token::Op(op) = self.token() else {
            return Ok(None);
        };
        let op = *op;
        let Some((left, right)) = binding_power(op) else {
            return Ok(None);
        };
        // Go locates a binary expression at its operator's token.
        let offset = self.position(self.pos) as u32;
        Ok((left >= min).then_some(Suffix::Binary(op, right, offset)))
    }
    fn member_name(&mut self) -> Result<Name> {
        match self.token() {
            Token::Word(name) if !name.starts_with('@') => {
                let name = *name;
                self.bump()?;
                Name::new(self.work, &name)
            }
            Token::Op("<=>") => {
                self.bump()?;
                Name::new(self.work, "<=>")
            }
            _ => self.expected(Label::Text("member name")),
        }
    }
    /// The index of the last token before the current one, skipping line breaks.
    fn previous_index(&self) -> Result<usize> {
        let mut index = self.pos;
        while index > 0 {
            self.work.charge(1)?;
            index -= 1;
            if self.tokens[index].token != Token::EndLine {
                return Ok(index);
            }
        }
        Ok(0)
    }
    fn previous(&self) -> Result<&Lexeme<'a>> {
        Ok(self
            .tokens
            .find((0..self.pos).rev(), self.work, |t| {
                t.token != Token::EndLine
            })?
            .unwrap())
    }
    fn can_attach_do(&self) -> bool {
        (self.command_depth == 0 || self.groups > self.command_group)
            && self.loop_condition.is_none_or(|group| self.groups > group)
    }
    fn continuation_position(&self, min: u8) -> Result<Option<usize>> {
        if self.token() != &Token::EndLine {
            return Ok(None);
        }
        let mut next = self.pos;
        while self.tokens[next].token == Token::EndLine {
            self.work.charge(1)?;
            if self.tokens[next].line == self.tokens[next].end_line {
                return Ok(None);
            }
            next += 1;
        }
        let lexeme = &self.tokens[next];
        let continues = match lexeme.token {
            Token::Word(ref word) if word == "do" => self.can_attach_do(),
            Token::P('.') | Token::Op("::" | "&.") => true,
            Token::P('?') => min <= 2,
            Token::P('(' | '[') => self.line_exprs == 0 && self.groups > 0,
            Token::Op(op) => {
                let Some((left, _)) = binding_power(op) else {
                    return Ok(None);
                };
                if left < min {
                    return Ok(None);
                }
                if self.line_exprs == 0 {
                    if op == "/" {
                        return Ok(None);
                    }
                    return Ok((self.groups > 0).then_some(next));
                }
                match op {
                    "+" | "-" => self
                        .tokens
                        .find(next + 1..self.tokens.len(), self.work, |t| {
                            t.token != Token::EndLine || t.line == t.end_line
                        })?
                        .is_some_and(|operand| {
                            !matches!(operand.token, Token::Eof | Token::EndLine)
                                && (operand.line > lexeme.end_line || operand.offset > lexeme.end)
                        }),
                    "*" => !self.splat_assignment_ahead(next)?,
                    "/" => false,
                    _ => true,
                }
            }
            _ => false,
        };
        Ok(continues.then_some(next))
    }
    fn splat_assignment_ahead(&self, start: usize) -> Result<bool> {
        self.work.charge(1)?;
        let mut groups = 0usize;
        let mut previous = &self.tokens[start];
        let operand = &self.tokens[start + 1];
        let shaped = matches!(operand.token, Token::P(',') | Token::Op("="))
            || operand.offset == previous.end;
        let mut comma = false;
        for token in self.tokens.from(start + 1) {
            self.work.charge(1)?;
            if token.line > self.tokens[start].line + 64 {
                return Ok(false);
            }
            if token.token == Token::EndLine {
                if token.line == token.end_line {
                    return Ok(false);
                }
                continue;
            }
            if token.line > previous.end_line
                && groups == 0
                && previous.token != Token::P(',')
                && !((shaped || comma)
                    && (token.token == Token::Op("=")
                        || (matches!(token.token, Token::P('.') | Token::Op("&."))
                            && matches!(previous.token, Token::Word(_) | Token::P(')' | ']')))))
            {
                return Ok(false);
            }
            if groups == 0 && token.token != Token::Op("=") {
                let allowed = if matches!(previous.token, Token::P('.') | Token::Op("&.")) {
                    matches!(token.token, Token::Word(_))
                } else {
                    match &token.token {
                        Token::Word(w) => !reserved(w),
                        Token::P(',' | '.' | '(' | ')' | '[' | ']') | Token::Op("*" | "&.") => true,
                        _ => false,
                    }
                };
                if !allowed {
                    return Ok(false);
                }
            }
            match token.token {
                Token::P('(' | '[') => groups += 1,
                Token::P(')' | ']') => {
                    let Some(next) = groups.checked_sub(1) else {
                        return Ok(false);
                    };
                    groups = next;
                }
                Token::Op("=") if groups == 0 => return Ok(true),
                Token::P(',') if groups == 0 => comma = true,
                Token::Eof => return Ok(false),
                _ => (),
            }
            previous = token;
        }
        Ok(false)
    }
    fn keyword_label(&self, pos: usize) -> bool {
        matches!(self.tokens[pos].token, Token::Word(_))
            && self
                .tokens
                .get(pos + 1)
                .is_some_and(|t| t.token == Token::P(':'))
    }
    fn command_start(&self, lhs: &Expr, min: u8) -> Result<bool> {
        self.work.charge(1)?;
        if self.line_exprs == 0
            || min > 14
            || !matches!(
                lhs.node,
                Node::Var(_) | Node::Member(..) | Node::SafeMember(..)
            )
        {
            return Ok(false);
        }
        let local = match &lhs.node {
            Node::Var(name) if name == "self" => return Ok(false),
            Node::Var(name) => self.locals.contains(self.work, name)?,
            _ => false,
        };
        let previous = self.previous()?;
        let next = &self.tokens[self.pos];
        if next.line != previous.end_line {
            return Ok(false);
        }
        if self.keyword_label(self.pos) {
            return Ok(true);
        }
        Ok(match next.token {
            Token::P(':')
                if self.ternaries.last() == Some(&self.groups)
                    && matches!(self.tokens[self.pos + 1].token, Token::Bytes(_)) =>
            {
                false
            }
            Token::P('[') => !local && previous.end != next.offset,
            // Only a declared local makes `%w` a modulo. An implicit block `it`
            // still calls a function named `it`, as in Go.
            Token::Words(..) => {
                let implicit =
                    matches!(&lhs.node, Node::Var(name) if name == "it") && !self.declared_it;
                (!local || implicit) && previous.end != next.offset
            }
            Token::Regex(..) => !local && previous.end != next.offset,
            Token::Op("/") => {
                !local
                    && previous.end != next.offset
                    && self
                        .source
                        .as_bytes()
                        .get(next.end)
                        .is_some_and(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
            }
            Token::Op(op @ ("*" | "**" | "&")) => {
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
        })
    }
    fn command_argument_start(&self, pos: usize, after_comma: bool) -> bool {
        if self.keyword_label(pos) {
            return true;
        }
        match &self.tokens[pos].token {
            Token::Word(w) if w == "then" => self.then_stop != Some(self.groups),
            Token::Word(w) => {
                !reserved(w) || matches!(w.as_str(), "case" | "for" | "yield" | "begin")
            }
            Token::Int(_)
            | Token::BigInt(..)
            | Token::Float(_)
            | Token::Bytes(_)
            | Token::Regex(..)
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
                && (matches!(&t.token, Token::Word(word) if !word.starts_with('@'))
                    || matches!(t.token, Token::Bytes(_))
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
                                | "=~"
                                | "!~"
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
    fn argument_kind(&mut self) -> Result<ArgumentKind> {
        Ok(if self.token() == &Token::Op("**") {
            self.bump()?;
            ArgumentKind::KeywordSplat
        } else if self.keyword_label(self.pos) {
            let Token::Word(name) = self.bump()? else {
                unreachable!()
            };
            self.bump()?;
            ArgumentKind::Keyword(Name::new(self.work, &name)?)
        } else if self.token() == &Token::Op("*") {
            self.bump()?;
            ArgumentKind::Splat
        } else {
            ArgumentKind::Positional
        })
    }
    fn literal_argument(
        &mut self,
        kind: &ArgumentKind,
        parenthesized: bool,
    ) -> Result<Option<Expr>> {
        self.work.charge(1)?;
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
            Token::Word(w) if w == "then" => self.then_stop != Some(self.groups),
            Token::Word(w) => {
                !reserved(w)
                    || matches!(
                        w.as_str(),
                        "if" | "unless" | "case" | "while" | "until" | "for" | "yield" | "begin"
                    )
            }
            Token::Int(_)
            | Token::BigInt(..)
            | Token::Float(_)
            | Token::Bytes(_)
            | Token::Regex(..)
            | Token::Template(_)
            | Token::Words(..) => true,
            Token::P('(' | '[' | '{' | ':') | Token::Op("+" | "-" | "!") => true,
            _ => false,
        }
    }
}

// Go nests a method's member access, or a function's name, below its call.
fn call_depth(callee: &Expr) -> u32 {
    match &callee.node {
        Node::Var(_) => 1,
        Node::Member(receiver, _) | Node::SafeMember(receiver, _) => 1 + receiver.depth,
        Node::Scope(receiver, _, None) => 1 + receiver.depth,
        _ => callee.depth,
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
        "==" | "!=" | "===" | "=~" | "!~" => (5, 6),
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

/// The lexer's message for a character no token starts with, which Go reports
/// as an invalid token.
const UNSUPPORTED_CHARACTER: &str = "unsupported character";

/// Go's rejection of a hash entry that is not a labeled or quoted key and its value.
const INVALID_HASH_PAIR: &str = "invalid hash pair: expected key like name: or \"name\":";

/// Go's diagnostic name for a token in a parse error.
#[derive(Clone, Copy)]
enum Label<'a> {
    Text(&'a str),
    Quoted(&'a str),
    Keyword(&'a str),
    Char(char),
}

impl<'a> Label<'a> {
    /// Names a word token: Go quotes its keywords, and spells the statement
    /// keywords its lexer lists by name in double quotes.
    fn word(w: &'a str) -> Self {
        match w {
            "def" | "class" | "enum" | "export" | "self" | "private" | "property" | "getter"
            | "setter" | "end" | "raise" | "return" | "yield" | "do" | "then" | "for" | "in"
            | "if" | "unless" | "elsif" | "else" | "true" | "false" | "nil" => Self::Keyword(w),
            _ if keyword(w) => Self::Quoted(w),
            _ => Self::Text("identifier"),
        }
    }
}

impl std::fmt::Display for Label<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Text(text) => f.write_str(text),
            Self::Quoted(text) => write!(f, "\"{text}\""),
            Self::Keyword(text) => write!(f, "'{text}'"),
            Self::Char(c) => write!(f, "\"{c}\""),
        }
    }
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
/// Every reserved word, sorted: the [`reserved`] words and the literal and
/// declaration words that cannot name a method or variable either.
pub(crate) const KEYWORDS: [&str; 34] = [
    "begin", "break", "case", "class", "def", "do", "else", "elsif", "end", "ensure", "enum",
    "export", "false", "for", "getter", "if", "in", "next", "nil", "private", "property", "raise",
    "rescue", "retry", "return", "self", "setter", "then", "true", "unless", "until", "when",
    "while", "yield",
];

pub(crate) fn keyword(w: &str) -> bool {
    KEYWORDS.binary_search(&w).is_ok()
}
pub(crate) fn unsupported(work: &dyn Work, message: &str) -> Error {
    crate::compilation::error(work, None, format_args!("{message}"))
}
