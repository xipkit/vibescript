use super::*;

#[derive(Debug)]
pub(crate) struct Expr {
    pub node: Node,
    pub(super) depth: u32,
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
    /// The height of the expression's syntax tree, which the parser bounds.
    pub(crate) fn height(&self) -> u32 {
        self.depth
    }

    /// Moves the node out of an expression, which cannot be destructured
    /// because it drops its subtree without recursion.
    pub(super) fn into_node(mut self) -> Node {
        std::mem::replace(&mut self.node, Node::Integer(0))
    }

    /// The offset of the first safe navigation in an assignment target's
    /// receiver chain, which Go reports there.
    pub(super) fn safe_navigation(&self) -> Option<usize> {
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
    pub(super) fn assignable(&self) -> bool {
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
    pub(super) fn depth(&self) -> u32 {
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
    // Destructuring nests as deeply as the syntax limit, so walk it without
    // recursion, in source order, keeping what is left of each level's
    // parts rather than every part at once, in a list reserved from `work`
    // as it grows.
    pub(super) fn parts(
        &self,
        work: &dyn Work,
        mut visit: impl FnMut(&Self, u32) -> Result<bool>,
    ) -> Result<bool> {
        // What is left of each level's parts, and its depth.
        type Level<'t> = (std::slice::Iter<'t, (Option<Target>, bool)>, u32);
        let mut levels: Buffer<Level<'_>> = Buffer::new();
        let mut next = Some((self, 0));
        loop {
            work.checkpoint()?;
            work.charge(1)?;
            let (target, depth) = match next.take() {
                Some(next) => next,
                None => {
                    let Some((level, depth)) = levels.last_mut() else {
                        return Ok(true);
                    };
                    let depth = *depth;
                    match level.next() {
                        Some((Some(target), _)) => (target, depth),
                        Some((None, _)) => continue,
                        None => {
                            levels.pop();
                            continue;
                        }
                    }
                }
            };
            if !visit(target, depth)? {
                return Ok(false);
            }
            match target {
                Self::Typed(target, _) => next = Some((target, depth)),
                Self::Value(_) => (),
                Self::Tuple(parts) => levels.push(work, (parts.iter(), depth + 1))?,
            }
        }
    }
    pub(super) fn is_binding(&self, work: &dyn Work) -> Result<bool> {
        self.parts(work, |target, _| {
            Ok(match target {
                Self::Value(e) => matches!(&e.node, Node::Var(name) if !name.starts_with('@')),
                _ => true,
            })
        })
    }
    pub(super) fn depth(&self, work: &dyn Work) -> Result<u32> {
        let mut deepest = 0;
        self.parts(work, |target, depth| {
            if let Self::Value(e) = target {
                deepest = deepest.max(depth + e.depth);
            } else if let Self::Tuple(_) = target {
                deepest = deepest.max(depth + 1);
            }
            Ok(true)
        })?;
        Ok(deepest)
    }
}

#[derive(Debug)]
pub(crate) struct Stmt {
    pub node: Statement,
    pub(super) depth: u32,
    pub offset: u32,
}

#[derive(Debug)]
pub(crate) enum Statement {
    Raise(Option<Boxed<Expr>>, Option<Boxed<Expr>>),
    Retry,
    /// A nested function declaration, which Go parses and then refuses to run.
    Unsupported,
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

impl Stmt {
    /// The height of the statement's syntax tree, which the parser bounds.
    pub(crate) fn height(&self) -> u32 {
        self.depth
    }
}

impl Statement {
    pub(super) fn at(self, work: &dyn Work, offset: u32) -> Result<Stmt> {
        Ok(Stmt {
            depth: self.depth(work)?,
            node: self,
            offset,
        })
    }
    // Match Go's syntax tree height; children carry their own heights. A
    // target's height is found with a walk that `work` reserves for.
    fn depth(&self, work: &dyn Work) -> Result<u32> {
        let body = |s: &[Stmt]| s.iter().map(|s| s.depth).max().unwrap_or(0);
        Ok(match self {
            Statement::Module(_)
            | Statement::UnboundClass(_)
            | Statement::Retry
            | Statement::Unsupported => 1,
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
            Statement::Assign(t, _, e) => 1 + t.depth(work)?.max(e.depth),
            Statement::If(branches, alternate, _) => {
                let branches = branches.iter().enumerate().map(|(i, (condition, body_))| {
                    // Each elsif is its own node beside the first branch.
                    u32::from(i > 0) + condition.depth.max(body(body_))
                });
                1 + branches.max().unwrap_or(0).max(body(alternate))
            }
            Statement::While(e, b, _) => 1 + e.depth.max(body(b)),
            Statement::For(t, e, b) => 1 + t.depth(work)?.max(e.depth).max(body(b)),
            Statement::Return(e) | Statement::Break(e) | Statement::Next(e) => {
                1 + e.as_ref().map_or(0, |e| e.depth)
            }
        })
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

/// A typed block parameter. Its name is a declaration only: `yield` and
/// `block_given?` are the only ways to reach the block. `&name?:` declares an
/// optional block, which the runtime treats like a required one: `yield`
/// without a block fails either way.
#[derive(Debug)]
pub(crate) struct BlockParam {
    pub name: Name,
    pub params: Buffer<crate::compilation::Type>,
    /// The block's result type; without one the block's value is discarded.
    pub result: Option<crate::compilation::Type>,
    /// The offset of the `&`.
    pub offset: u32,
}

/// A type alias, `type Name = T`, declared at the top level or in a module
/// or class body.
#[derive(Debug)]
pub(crate) struct TypeAlias {
    pub name: Name,
    pub ty: crate::compilation::Type,
    pub offset: u32,
}

impl Definition {
    pub(super) fn depth(&self) -> u32 {
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
    pub additions: typed::Additions,
    pub outline: Buffer<Outline>,
    /// The byte span of every string interpolation's content, for tooling
    /// that reports positions relative to an interpolation as Go does.
    pub interpolations: Buffer<(u32, u32)>,
    /// What the interpolations hold beside the source's own tokens, when
    /// it was parsed with them.
    pub interpolated: Interpolated,
}

/// What a source's string interpolations hold, at every depth, which a
/// pass that lexes each interpolation again reads beside the source's own
/// tokens.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Interpolated {
    /// The tokens inside them, each one's end among them.
    pub tokens: usize,
    /// What the payloads of those tokens hold once the tooling lists them,
    /// as an ordinary token's do: strings, symbols' names and percent
    /// literals' entries; and the spans of the interpolations nested in
    /// them.
    pub bytes: usize,
    /// The bytes of the identifiers among those tokens.
    pub words: usize,
    /// The entries of the percent literals among those tokens.
    pub entries: usize,
    /// The bytes of those entries without interpolations of their own.
    pub rewritten: usize,
}

/// A top-level declaration's kind, name and source byte range, in source order.
pub(crate) struct Outline {
    pub kind: crate::DeclarationKind,
    pub name: Name,
    pub start: usize,
    pub end: usize,
}
