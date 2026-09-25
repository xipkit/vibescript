//! A syntax tree that keeps every construct's source span, so rewrites can
//! edit the text around it and leave comments and layout alone.
//!
//! The tree records each construct's tokens whether or not a rule reads them
//! yet, so new rules need no parser changes.
#![allow(dead_code)]

use std::ops::Range;
use vibescript::tooling::TokenKind;

/// A token index into [`Tree::tokens`].
pub(crate) type Tok = usize;

/// A token with the line its last byte is on.
#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub end_line: usize,
}

/// A parsed source.
#[derive(Debug)]
pub(crate) struct Tree {
    pub tokens: Vec<Token>,
    pub body: Vec<Stmt>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub(crate) struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn range(self) -> Range<usize> {
        self.start..self.end
    }
}

#[derive(Debug)]
pub(crate) struct Stmt {
    pub span: Span,
    pub kind: StmtKind,
}

#[derive(Debug)]
pub(crate) enum StmtKind {
    Expr(Expr),
    Assign(Assign),
    If(If),
    While(While),
    For(For),
    /// A statement followed by `if`, `unless`, `while` or `until` and a condition.
    Modifier(Modifier),
    /// `return`, `break` or `next`, and its value.
    Flow(Tok, Option<Expr>),
    Raise(Tok, Option<Expr>, Option<Expr>),
    Retry(Tok),
    Def(Box<Def>),
    Class(Box<Class>),
    Enum(Enum),
    /// A declaration this migration leaves alone, such as an alias.
    Other,
}

#[derive(Debug)]
pub(crate) struct Assign {
    pub targets: Vec<Target>,
    /// The assignment operator, such as `=` or `+=`.
    pub op: Tok,
    pub values: Vec<Expr>,
}

#[derive(Debug)]
pub(crate) enum Target {
    Expr(Expr),
    /// `*name`, or a bare `*`.
    Splat(Tok, Option<Box<Target>>),
    /// A parenthesized or bracketed group of targets.
    Group(Span, Vec<Target>),
    /// A target with a type annotation, as in a block parameter.
    Typed(Box<Target>, TypeExpr),
}

impl Target {
    pub fn span(&self) -> Span {
        match self {
            Self::Expr(expr) => expr.span,
            Self::Splat(_, _) | Self::Group(..) => self.outer_span(),
            Self::Typed(target, ty) => Span {
                start: target.span().start,
                end: ty.span.end,
            },
        }
    }

    fn outer_span(&self) -> Span {
        match self {
            Self::Group(span, _) => *span,
            Self::Splat(_, Some(inner)) => inner.span(),
            _ => Span::default(),
        }
    }

    /// Visits the names this target binds.
    pub fn names(&self, visit: &mut impl FnMut(&str, &Expr)) {
        match self {
            Self::Expr(expr) => {
                if let ExprKind::Name(name) = &expr.kind {
                    visit(name, expr);
                }
            }
            Self::Splat(_, Some(inner)) | Self::Typed(inner, _) => inner.names(visit),
            Self::Splat(_, None) => (),
            Self::Group(_, parts) => {
                for part in parts {
                    part.names(visit);
                }
            }
        }
    }
}

#[derive(Debug)]
pub(crate) struct If {
    /// The `if` or `unless` keyword.
    pub keyword: Tok,
    pub unless: bool,
    /// Each condition and its body; `elsif` branches follow the first.
    pub branches: Vec<(Expr, Vec<Stmt>)>,
    pub alternate: Option<(Tok, Vec<Stmt>)>,
    pub end: Tok,
}

#[derive(Debug)]
pub(crate) struct While {
    pub keyword: Tok,
    pub until: bool,
    pub condition: Expr,
    pub body: Vec<Stmt>,
    pub end: Tok,
}

#[derive(Debug)]
pub(crate) struct For {
    pub target: Target,
    pub iterable: Expr,
    pub body: Vec<Stmt>,
}

#[derive(Debug)]
pub(crate) struct Modifier {
    pub body: Box<Stmt>,
    pub keyword: Tok,
    pub kind: ModifierKind,
    pub condition: Expr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ModifierKind {
    If,
    Unless,
    While,
    Until,
}

#[derive(Debug)]
pub(crate) struct Def {
    pub keyword: Tok,
    /// `private` or `export` before `def`.
    pub modifier: Option<Tok>,
    pub name: String,
    pub name_span: Span,
    pub class_method: bool,
    pub params: Vec<Param>,
    /// A typed block parameter, `&block: A -> R`, already declared.
    pub block: Option<Span>,
    /// The parameter list's parentheses.
    pub parens: Option<(Tok, Tok)>,
    /// The `->` and the declared result type.
    pub result: Option<(Tok, TypeExpr)>,
    pub body: Vec<Stmt>,
    pub rescue: Option<Rescued>,
    pub end: Tok,
}

impl Def {
    /// The offset the compiler records for the function: its `def`.
    pub fn offset(&self, tokens: &[Token]) -> usize {
        tokens[self.keyword].start
    }
}

#[derive(Debug)]
pub(crate) struct Param {
    pub kind: ParamKind,
    pub name: String,
    pub name_tok: Tok,
    pub instance: bool,
    pub ty: Option<TypeExpr>,
    pub default: Option<Expr>,
    /// The span from the sigil or name through the default.
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamKind {
    Positional,
    /// A keyword parameter; `required` when it has no default.
    Keyword,
    Rest,
    KeywordRest,
}

/// The rescue, else and ensure clauses of a function or `begin`.
#[derive(Debug)]
pub(crate) struct Rescued {
    pub rescues: Vec<RescueClause>,
    pub alternate: Option<Vec<Stmt>>,
    pub ensure: Option<Vec<Stmt>>,
}

#[derive(Debug)]
pub(crate) struct RescueClause {
    pub keyword: Tok,
    pub binding: Option<String>,
    pub body: Vec<Stmt>,
}

#[derive(Debug)]
pub(crate) struct Class {
    pub keyword: Tok,
    pub module: bool,
    pub name: String,
    pub name_tok: Tok,
    pub members: Vec<Member>,
    pub end: Tok,
}

#[derive(Debug)]
pub(crate) enum Member {
    Def(Def),
    Property(Property),
    /// An instance-variable declaration, `@name: T` or `@name: T = value`.
    Ivar(String, TypeExpr, Option<Expr>),
    Class(Class),
    Stmt(Stmt),
    /// A visibility directive, alias or other declaration.
    Other(Span),
}

/// A `property`, `getter` or `setter` declaration.
#[derive(Debug)]
pub(crate) struct Property {
    pub keyword: Tok,
    /// Each declared name and its type, if any.
    pub names: Vec<(Tok, Option<TypeExpr>)>,
    pub span: Span,
}

#[derive(Debug)]
pub(crate) struct Enum {
    pub name: String,
    pub members: Vec<String>,
}

/// A type annotation's span and structure.
#[derive(Clone, Debug)]
pub(crate) struct TypeExpr {
    pub span: Span,
    pub kind: TypeKind,
    pub nullable: bool,
}

#[derive(Clone, Debug)]
pub(crate) enum TypeKind {
    /// A named type and its name token, with type arguments if any.
    Named(Tok, Vec<TypeExpr>),
    /// A dotted name, such as `Outer.Inner`.
    Qualified(Vec<Tok>),
    Shape(Vec<(Tok, TypeExpr)>, bool),
    Union(Vec<TypeExpr>),
    /// `[A, B]`: an array of exactly these elements.
    Tuple(Vec<TypeExpr>),
}

#[derive(Debug)]
pub(crate) struct Expr {
    pub span: Span,
    pub kind: ExprKind,
}

#[derive(Debug)]
pub(crate) enum ExprKind {
    Nil,
    True,
    False,
    SelfRef,
    Integer,
    Float,
    Str,
    /// A string with interpolation, and each interpolation's expression
    /// when it parsed.
    Template(Vec<Option<Expr>>),
    Symbol,
    Regex,
    Words,
    /// An identifier: a local, a constant or a call without arguments.
    Name(String),
    /// An instance or class variable.
    Ivar(String),
    Array(Vec<Expr>),
    Hash(Vec<Entry>),
    /// A type literal that is not also a hash, such as `array<int>`.
    TypeLiteral,
    Unary(Tok, Box<Expr>),
    Binary(Tok, Box<Expr>, Box<Expr>),
    Range(Option<Box<Expr>>, Tok, Option<Box<Expr>>),
    Ternary(Box<Expr>, Tok, Box<Expr>, Box<Expr>),
    Call(Box<Call>),
    /// A call of a computed value, such as `(f)(1)`.
    Computed(Box<Expr>, Args),
    /// A block attached to something other than a named call.
    BlockCall(Box<Expr>, Box<Block>),
    Index(Box<Expr>, Tok, Vec<Expr>, Tok),
    Yield(Tok, Option<Args>),
    Group(Tok, Box<Expr>, Tok),
    If(Box<If>),
    Case(Box<Case>),
    /// A loop used as an expression.
    Loop(Box<Stmt>),
    Begin(Box<Begin>),
    /// `body rescue fallback`.
    Rescue(Box<Expr>, Tok, Box<Expr>),
}

#[derive(Debug)]
pub(crate) struct Entry {
    pub key: Tok,
    pub name: Vec<u8>,
    /// Written as `name` alone, taking the local of that name.
    pub shorthand: bool,
    pub value: Expr,
}

/// A named call: a function, member or scoped call, with its arguments and block.
#[derive(Debug)]
pub(crate) struct Call {
    pub receiver: Option<Expr>,
    /// `.`, `&.` or `::` before the name.
    pub operator: Option<Tok>,
    pub name: String,
    pub name_tok: Tok,
    pub args: Option<Args>,
    pub block: Option<Block>,
}

impl Call {
    pub fn safe(&self, tokens: &[Token]) -> bool {
        self.operator
            .is_some_and(|op| tokens[op].kind == TokenKind::Operator("&."))
    }

    pub fn scoped(&self, tokens: &[Token]) -> bool {
        self.operator
            .is_some_and(|op| tokens[op].kind == TokenKind::Operator("::"))
    }

    pub fn argument_count(&self) -> usize {
        self.args.as_ref().map_or(0, |args| args.items.len())
    }
}

#[derive(Debug)]
pub(crate) struct Args {
    pub parens: Option<(Tok, Tok)>,
    pub items: Vec<Arg>,
}

#[derive(Debug)]
pub(crate) struct Arg {
    pub kind: ArgKind,
    /// The span of the whole argument, including a label or sigil.
    pub span: Span,
    pub value: Expr,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ArgKind {
    Positional,
    Splat,
    Keyword(String),
    KeywordSplat,
}

#[derive(Debug)]
pub(crate) struct Block {
    /// The `do` or `{`.
    pub open: Tok,
    pub brace: bool,
    pub params: Vec<Target>,
    /// The `|` pipes, when written.
    pub pipes: Option<(Tok, Tok)>,
    pub body: Vec<Stmt>,
    /// The `end` or `}`.
    pub close: Tok,
}

#[derive(Debug)]
pub(crate) struct Case {
    pub keyword: Tok,
    pub subject: Option<Expr>,
    pub whens: Vec<When>,
    pub alternate: Option<(Tok, Expr)>,
    pub end: Tok,
}

#[derive(Debug)]
pub(crate) struct When {
    pub keyword: Tok,
    /// Each matcher, and whether it is splatted.
    pub values: Vec<(Expr, bool)>,
    pub result: Expr,
}

#[derive(Debug)]
pub(crate) struct Begin {
    pub keyword: Tok,
    pub body: Vec<Stmt>,
    pub rescued: Rescued,
    pub end: Tok,
}
