use crate::{
    Error, Result, Value,
    compilation::{Boxed, Buffer, Bytes, Name, Table, Task, Tasks, Text, Work},
};
use std::cell::{RefCell, RefMut};

mod classes;
mod declarations;
mod errors;
mod lexer;
pub(crate) mod modules;
pub(crate) mod record;
mod recovery;
mod teardown;
mod tokens;
pub(crate) mod typed;
mod types;
pub(crate) mod unicode;
mod work;
use lexer::{Failure, Lexeme, Part, Token, lex};
use tokens::Tokens;

// Go's maxSyntaxDepth bounds both parser recursion and syntax tree height.
const MAX_DEPTH: usize = 1024;
pub(crate) const MAX_SOURCE: usize = 8 << 20;
pub(crate) const TOO_DEEP: &str = "syntax nesting too deep";
const ADJACENT_EXPRESSIONS: &str = "adjacent expressions need a separator; insert an operator, a comma between arguments, or a newline (or `;`) between statements";

/// Reports the `?` or `!` at `offset` in `source`. A suffix that ends the
/// name has a repair; one inside a name, as in `x?1`, has none, since
/// removing it could name something else.
pub(crate) fn name_suffix_error(work: &dyn Work, source: &str, offset: usize) -> Error {
    use crate::diagnostic::{Code, Diagnostic, Fix, Span};
    let span = Span::new(offset, offset + 1);
    let Some((label, replacement)) = suffix_repair(source, offset) else {
        let message = "`?` and `!` may only end a method name";
        let diagnostic = Diagnostic::error(Code::NAME_SUFFIX, span, message);
        return Error::syntax(work, offset, message).with_diagnostic(diagnostic);
    };
    let message = "only method names may end in `?` or `!`; remove the suffix from this name";
    let diagnostic = Diagnostic::error(Code::NAME_SUFFIX, span, message).with_fix(Fix::replace(
        label,
        span,
        replacement,
    ));
    Error::syntax(work, offset, message).with_diagnostic(diagnostic)
}

/// The repair of the suffix at `offset`, with its label: removal, or an
/// underscore where the name would become a keyword that cannot stand there.
/// None when the name continues after its `?` and `!` characters.
fn suffix_repair(source: &str, offset: usize) -> Option<(&'static str, &'static str)> {
    let rest = source[offset..].trim_start_matches(['?', '!']);
    if rest.starts_with(|c: char| c == '_' || unicode::letter_or_digit(c)) {
        return None;
    }
    let start = source[..offset]
        .rfind(|c: char| c != '_' && !unicode::letter_or_digit(c))
        .map_or(0, |i| i + source[i..].chars().next().unwrap().len_utf8());
    Some(
        if keyword(&source[start..offset]) && !keyword_allowed(&source[..start]) {
            ("replace the name suffix with an underscore", "_")
        } else {
            ("remove the name suffix", "")
        },
    )
}

/// Whether a keyword can be the name that follows `before`: a variable's
/// after its sigil, a symbol's, or a member's, except in `def self.name`.
fn keyword_allowed(before: &str) -> bool {
    let before = before.trim_end();
    if before.ends_with(['@', ':']) {
        return true;
    }
    let Some(receiver) = before.strip_suffix('.').map(str::trim_end) else {
        return false;
    };
    !receiver
        .strip_suffix("self")
        .is_some_and(|rest| rest.trim_end().ends_with("def"))
}

/// The registration a host-supplied name comes from, as its errors name it.
/// Such a name has no position in any script, so its errors carry no fix.
#[derive(Clone, Copy)]
pub(crate) struct HostName<'a> {
    kind: &'a str,
    owner: Option<(&'a str, &'a str)>,
    value: bool,
}

impl<'a> HostName<'a> {
    /// A function registered on the engine.
    pub(crate) const FUNCTION: Self = Self::new("host function");
    /// A global declared on the engine or supplied by a call.
    pub(crate) const GLOBAL: Self = Self::new("global");
    /// A capability's root name.
    pub(crate) const CAPABILITY: Self = Self::new("capability");
    /// A method in a host object that reaches a script.
    pub(crate) const METHOD: Self = Self::new("method");

    const fn new(kind: &'a str) -> Self {
        Self {
            kind,
            owner: None,
            value: false,
        }
    }

    /// A method in the value this registration binds to `name`.
    pub(crate) fn member_of(self, name: &'a str) -> Self {
        Self {
            owner: Some((self.kind, name)),
            ..Self::METHOD
        }
    }

    /// The same registration, known to be bound to the value being checked.
    pub(crate) fn bound(self) -> Self {
        Self {
            value: true,
            ..self
        }
    }

    fn error(self, name: &[u8], reason: impl std::fmt::Display) -> Error {
        use std::fmt::Write;
        // Enough bytes for `source_text` to mark a cut.
        let shown = String::from_utf8_lossy(&name[..name.len().min(72)]);
        let mut message = format!("invalid {} name \"{}\"", self.kind, source_text(&shown));
        if let Some((kind, owner)) = self.owner {
            let _ = write!(message, " in {kind} \"{}\"", source_text(owner));
        }
        let _ = write!(message, ": {reason}");
        Error::new(crate::ErrorKind::Argument, message)
    }
}

/// Checks a host binding against the same suffix rule as source bindings.
pub(crate) fn binding_name(work: &dyn Work, host: HostName<'_>, name: &str) -> Result<()> {
    work.checkpoint()?;
    work.bytes(name.len())?;
    if name_suffix_position(name).is_some() {
        let reason = if host.value {
            "only method names may end in `?` or `!`, and this value is not callable"
        } else {
            "only method names may end in `?` or `!`"
        };
        return Err(host.error(name.as_bytes(), reason));
    }
    Ok(())
}

fn name_suffix_position(name: &str) -> Option<usize> {
    memchr::memchr2(b'?', b'!', name.as_bytes())
}

/// The operators an instance answers with the method of the same name: every
/// binary operator that is not short-circuiting, `<<`, and indexing. Each is
/// paired with whether `def` can spell it; an alias may name any of them.
const OPERATOR_METHODS: [(&str, bool); 21] = [
    ("+", true),
    ("-", true),
    ("*", true),
    ("/", true),
    ("//", false),
    ("%", true),
    ("**", true),
    ("<<", true),
    ("&", true),
    ("==", true),
    ("!=", true),
    ("===", false),
    ("=~", false),
    ("!~", false),
    ("<", true),
    ("<=", true),
    (">", true),
    (">=", true),
    ("<=>", true),
    ("[]", true),
    ("[]=", true),
];

/// Operators a symbol can spell that never call a method: `!`, `&&` and `||`
/// act on bools, and `|` is no binary operator.
const UNDISPATCHED_OPERATORS: [&str; 4] = ["!", "&&", "||", "|"];

/// Whether `def` can define the operator method `op`.
pub(super) fn def_operator(op: &str) -> bool {
    OPERATOR_METHODS.contains(&(op, true))
}

/// A method spelling rejected before a callable can be published.
enum MethodNameError {
    Suffix(usize),
    Undispatched(&'static str),
    Invalid,
}

impl MethodNameError {
    pub(crate) fn diagnostic(&self, work: &dyn Work, source: &str, offset: usize) -> Error {
        match self {
            Self::Suffix(suffix) => name_suffix_error(work, source, offset + suffix),
            _ => Error::syntax(work, offset, self.message()),
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Undispatched(op) => {
                format!(
                    "`{op}` cannot name a method: `!`, `&&`, `||` and `|` never dispatch to methods"
                )
            }
            _ => "invalid method name".to_owned(),
        }
    }

    /// Why a host's method name is invalid, with no source to repair.
    fn reason(&self) -> String {
        match self {
            Self::Suffix(_) => {
                "a method name may end in one `?` or `!`, and has neither elsewhere".to_owned()
            }
            Self::Undispatched(_) => self.message(),
            Self::Invalid => HOST_METHOD_SPELLING.to_owned(),
        }
    }
}

const HOST_METHOD_SPELLING: &str = "a method name starts with a letter or `_`, continues with letters, digits and `_`, and may end in one `?`, `!` or `=`";

/// Validates callable spellings, including operator methods and setters.
/// Lexer identifiers already satisfy the character rule; host names and decoded
/// symbols need that check too.
fn method_spelling(name: &str, lexed: bool) -> std::result::Result<(), MethodNameError> {
    if OPERATOR_METHODS.iter().any(|(op, _)| *op == name) {
        return Ok(());
    }
    if let Some(op) = UNDISPATCHED_OPERATORS.iter().find(|op| **op == name) {
        return Err(MethodNameError::Undispatched(op));
    }
    let stem = if let Some(suffix) = name_suffix_position(name) {
        if suffix + 1 != name.len() || name.starts_with('@') {
            return Err(MethodNameError::Suffix(suffix));
        }
        &name[..suffix]
    } else {
        name.strip_suffix('=').unwrap_or(name)
    };
    if lexed {
        return Ok(());
    }
    let mut chars = stem.chars();
    if !chars.next().is_some_and(|c| c == '_' || unicode::letter(c))
        || !chars.all(|c| c == '_' || unicode::letter_or_digit(c))
    {
        return Err(MethodNameError::Invalid);
    }
    Ok(())
}

/// Validates a host function that scripts must be able to call without a receiver.
pub(crate) fn host_function_name(work: &dyn Work, host: HostName<'_>, name: &str) -> Result<()> {
    host_method_name(work, host, name.as_bytes())?;
    if keyword(name) {
        return Err(host.error(name.as_bytes(), "a keyword cannot name a host function"));
    }
    if name.ends_with('=') {
        return Err(host.error(
            name.as_bytes(),
            "a setter is called through a receiver, so it cannot be a host function",
        ));
    }
    Ok(())
}

/// Checks a host method's published name; its descriptor's diagnostic label
/// is separate. Setters such as `value=` are methods too, as a module's or a
/// class's `def value=` is.
pub(crate) fn host_method_name(work: &dyn Work, host: HostName<'_>, key: &[u8]) -> Result<()> {
    work.checkpoint()?;
    work.bytes(key.len())?;
    let Ok(name) = std::str::from_utf8(key) else {
        return Err(host.error(key, "method names must be UTF-8"));
    };
    method_spelling(name, false).map_err(|error| host.error(key, error.reason()))?;
    if !name.starts_with(|c: char| c == '_' || unicode::letter(c)) {
        return Err(host.error(key, HOST_METHOD_SPELLING));
    }
    Ok(())
}

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
    /// The height of the expression's syntax tree, which the parser bounds.
    pub(crate) fn height(&self) -> u32 {
        self.depth
    }

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
    pub additions: typed::Additions,
    pub outline: Buffer<Outline>,
    /// The byte span of every string interpolation's content, for tooling
    /// that reports positions relative to an interpolation as Go does.
    pub interpolations: Buffer<(u32, u32)>,
    /// The spans of suffixed bare reads that a binding in scope would
    /// satisfy without the suffix, sorted; see [`suffix_read_fixes`].
    pub suffix_reads: Buffer<(u32, u32)>,
}

/// A top-level declaration's kind, name and source byte range, in source order.
pub(crate) struct Outline {
    pub kind: crate::DeclarationKind,
    pub name: Name,
    pub start: usize,
    pub end: usize,
}

/// Repairs an undefined suffixed read, such as `ok?` once V0003 has renamed
/// the parameter `ok?` to `ok`, by reading the binding in scope that its
/// name without the suffix names. Only bare reads recorded while parsing
/// `source` qualify, never calls, so the repair cannot pick another method.
pub(crate) fn suffix_read_fixes(
    work: &dyn Work,
    source: &str,
    parsed: &Declarations,
    diagnostics: &mut [crate::diagnostic::Diagnostic],
) -> Result<()> {
    use crate::diagnostic::{Code, Fix, Span};
    if parsed.suffix_reads.is_empty() {
        return Ok(());
    }
    work.charge(diagnostics.len())?;
    for diagnostic in diagnostics {
        if diagnostic.code != Code::UNDEFINED_NAME
            || diagnostic.file.is_some()
            || !diagnostic.fixes.is_empty()
        {
            continue;
        }
        let Span { start, end } = diagnostic.span;
        let (Ok(first), Ok(last)) = (u32::try_from(start), u32::try_from(end)) else {
            continue;
        };
        if parsed.suffix_reads.binary_search(&(first, last)).is_err() {
            continue;
        }
        let stem = &source[start..end - 1];
        let replacement = if keyword(stem) { "_" } else { "" };
        diagnostic.fixes.push(Fix::replace(
            format!("read `{stem}{replacement}`"),
            Span::new(end - 1, end),
            replacement,
        ));
    }
    Ok(())
}

fn parser<'a>(source: &'a str, work: &'a dyn crate::compilation::Work) -> Result<Parser<'a>> {
    Ok(parser_from_tokens(
        source,
        work,
        Tokens::new(lex(source, work)?, work)?,
    ))
}

fn parser_from_tokens<'a>(source: &'a str, work: &'a dyn Work, tokens: Tokens<'a>) -> Parser<'a> {
    Parser::with_type_names(Parser {
        work,
        source,
        lex_depth: 0,
        tokens,
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
        type_call: false,
        type_argument: false,
        type_structural_error: false,
        interpolations: Buffer::new(),
        suffix_reads: Buffer::new(),
        record: None,
        inside_class: false,
        nesting: 0,
        call_end: 0,
        percent_argument: 0,
        block_name: None,
        type_names: Table::new(),
        alias_names: Table::new(),
        additions: typed::Additions::default(),
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

pub(crate) use record::parse_with_tokens;

thread_local! {
    /// Whether the parse in progress reads only the canonical surface of
    /// ADR-008, as [`canonical_error`] asks.
    static CANONICAL: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether the parse in progress reads only the canonical surface: `do`
/// opens no block, `unless` and `until` start nothing, `%` never starts a
/// percent literal, and a keyword parameter follows a bare `*` or a rest
/// parameter.
fn canonical() -> bool {
    CANONICAL.with(std::cell::Cell::get)
}

/// The syntax error `source` has in the canonical surface of ADR-008. The
/// full grammar still reads the removed syntax, only so that a well-formed
/// use of it reaches the checker, which reports it with a fix; a source that
/// does not parse either way reports the error the canonical grammar finds,
/// where the removed syntax is no syntax at all, and so does a source the
/// checker's surface rules cannot read. Returns `None` when the canonical
/// parse succeeds.
pub(crate) fn canonical_error(source: &str, work: &dyn crate::compilation::Work) -> Option<Error> {
    canonical_error_mode(source, work, work.unmetered())
}

fn canonical_error_mode(source: &str, work: &dyn Work, recover: bool) -> Option<Error> {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            CANONICAL.with(|canonical| canonical.set(self.0));
        }
    }
    let _restore = Restore(CANONICAL.with(|canonical| canonical.replace(true)));
    if !recover {
        return parse(source, work).err();
    }
    let parser = match parser(source, work) {
        Ok(parser) => parser,
        Err(error) => return Some(error),
    };
    let parsing = Parsing::<recovery::FailFast>::new(parser);
    let error = parsing.run(Call::Program).err()?;
    let tokens = parsing.parser.into_inner().tokens.original();
    Some(recovery::diagnostics_with_tokens(
        source, error, work, tokens,
    ))
}

/// Replaces a syntax error of the full grammar with the one the canonical
/// surface reports for `source` ([`canonical_error`]); other errors, such
/// as an exhausted quota, stand.
pub(crate) fn canonical_syntax(
    source: &str,
    work: &dyn crate::compilation::Work,
    error: Error,
) -> Error {
    canonical_syntax_mode(source, work, error, work.unmetered())
}

/// Recovers host compilation errors under the caller's quotas and interruption.
pub(crate) fn host_syntax(source: &str, work: &dyn Work, error: Error) -> Error {
    canonical_syntax_mode(source, work, error, true)
}

fn canonical_syntax_mode(source: &str, work: &dyn Work, error: Error, recover: bool) -> Error {
    if error.kind != crate::ErrorKind::Syntax {
        return error;
    }
    canonical_error_mode(source, work, recover).unwrap_or_else(|| {
        if recover {
            recovery::diagnostics(source, error, work)
        } else {
            error
        }
    })
}

pub(crate) fn parse(source: &str, work: &dyn crate::compilation::Work) -> Result<Declarations> {
    let parsing = Parsing::<recovery::FailFast>::new(parser(source, work)?);
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
    /// Whether the call whose arguments come next takes types, as `as` and
    /// `JSON.parse_as` do, where `[int, string]` is a tuple type.
    type_call: bool,
    /// Whether the braced group that comes next is an argument of a call
    /// that takes types, where a shape may name the source's classes and
    /// enums.
    type_argument: bool,
    type_structural_error: bool,
    interpolations: Buffer<(u32, u32)>,
    /// Bare reads such as `ok?` whose name without the suffix, as V0003
    /// repairs it, is a binding in scope: a fix for them when nothing else
    /// defines the suffixed name.
    suffix_reads: Buffer<(u32, u32)>,
    /// Tooling facts, collected only by [`record::parse`].
    record: Option<Box<record::Record>>,
    /// Whether a class or module body encloses the current statement, as Go
    /// tracks it for declarations and operator methods.
    inside_class: bool,
    /// How many statement blocks enclose the current statement.
    nesting: usize,
    /// The token after the `)` of the latest parenthesized call with
    /// arguments, where Go attaches a `do` block from the next line.
    call_end: usize,
    /// The ambiguous percent literal a command call takes as its argument.
    percent_argument: usize,
    /// The typed block parameter of the function being parsed, whose name is
    /// a declaration only.
    block_name: Option<Name>,
    /// The type aliases, classes and enums the source declares anywhere,
    /// which read as types where an expression could also be meant.
    type_names: Table<()>,
    /// The type aliases among [`Self::type_names`].
    alias_names: Table<()>,
    /// The typed declarations parsed so far that live beside the tree.
    additions: typed::Additions,
}

/// Where a destructuring target list appears.
#[derive(Clone, Copy, PartialEq)]
enum Place {
    Statement,
    For,
    /// A parenthesized or bracketed group, and whether its names take types.
    Group(bool),
}

/// Writes a destructuring target as Go's `FormatDestructureTarget` does.
fn target_text(target: &Target, out: &mut Vec<u8>) -> Result<()> {
    match target {
        Target::Value(Expr {
            node: Node::Var(name),
            ..
        }) => out.extend_from_slice(name.as_bytes()),
        Target::Tuple(parts) => {
            out.push(b'(');
            for (index, (part, rest)) in parts.iter().enumerate() {
                if index > 0 {
                    out.extend_from_slice(b", ");
                }
                if *rest {
                    out.push(b'*');
                }
                if let Some(part) = part {
                    target_text(part, out)?;
                }
            }
            out.push(b')');
        }
        Target::Typed(target, ty) => {
            target_text(target, out)?;
            out.extend_from_slice(b": ");
            crate::shapes::format(ty, out)?;
        }
        Target::Value(_) => (),
    }
    Ok(())
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
struct Parsing<'a, M: recovery::Mode = recovery::FailFast> {
    parser: RefCell<Parser<'a>>,
    tasks: Tasks<Call, Parsed>,
    recovery: RefCell<M::State>,
}

impl<'a, M: recovery::Mode> Parsing<'a, M> {
    fn new(parser: Parser<'a>) -> Self {
        Self {
            parser: RefCell::new(parser),
            tasks: Tasks::new(),
            recovery: RefCell::new(M::State::default()),
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
                let result = self.expr_tail(lhs, min, Some(suffix), None, false).await;
                self.p().depth -= 1;
                Ok(Parsed::Expr(result?))
            }),
            Call::Block(stop) => {
                Box::pin(async move { Ok(Parsed::Body(self.block_task(stop).await?)) })
            }
            Call::Target(typed) => Box::pin(async move {
                let (target, tuple) = self.target(Place::Group(typed)).await?;
                Ok(Parsed::Target(target, tuple))
            }),
            Call::Class => Box::pin(async { Ok(Parsed::Module(self.class_like(false).await?)) }),
            Call::Module => Box::pin(async { Ok(Parsed::Module(self.class_like(true).await?)) }),
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

    /// Attaches a block to a whole statement-level expression, as Go's
    /// `canAttachPeekBlock` does after a line expression: a `do` block from
    /// any line, or a brace block on the same line after a call.
    async fn trailing_block(&self, expr: Expr) -> Result<Expr> {
        let brace = {
            let mut p = self.p();
            let next = p.significant(p.pos);
            let brace = match &p.tokens[next].token {
                Token::Word(w) if w == "do" && !canonical() => {
                    (next != p.pos || p.can_attach_do()).then_some(false)
                }
                Token::P('{') => (next == p.pos && p.block_follows(&expr)?).then_some(true),
                _ => None,
            };
            if brace.is_some() {
                p.pos = next;
            }
            brace
        };
        let Some(brace) = brace else {
            return Ok(expr);
        };
        Box::pin(self.block_expression(expr, brace)).await
    }

    /// Parses a line expression that may take a `do` block from a later line.
    async fn block_line_expr(&self) -> Result<Expr> {
        let expr = self.line_expr(0).await?;
        self.trailing_block(expr).await
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

    async fn block_task(&self, stop: &[&str]) -> Result<Buffer<Stmt>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut body = Buffer::new();
        self.p().nesting += 1;
        loop {
            {
                let mut p = self.p();
                p.lines()?;
                if matches!(p.token(),Token::Word(w) if stop.contains(&w.as_str()))
                    || (p.token() == &Token::P('}') && stop.contains(&"}"))
                {
                    break;
                }
                if matches!(p.token(), Token::Eof) {
                    return p.expected(Label::Text(if stop.contains(&"}") { "}" } else { "end" }));
                }
            }
            let checkpoint = self.recovery_checkpoint();
            match self.statement().await {
                Ok(statement) => body.push(work, statement)?,
                Err(error) => self.recover(checkpoint, stop, error)?,
            }
        }
        self.p().nesting -= 1;
        Ok(body)
    }

    // A condition ends at `then`, which otherwise names a local like any
    // identifier, as in Go.
    async fn condition(&self) -> Result<Expr> {
        let previous = {
            let mut p = self.p();
            p.line_breaks()?;
            let groups = p.groups;
            p.then_stop.replace(groups)
        };
        let condition = self.line_expr(0).await;
        self.p().then_stop = previous;
        condition
    }

    /// Parses a statement that declares nothing.
    async fn plain(&self) -> Result<Stmt> {
        let offset = {
            let mut p = self.p();
            p.work.charge(1)?;
            p.enter()?;
            p.tokens[p.pos].offset as u32
        };
        let stmt = self.modified_statement(offset).await?.at(offset);
        let mut p = self.p();
        p.expression_separator()?;
        p.check_depth(stmt.depth, stmt.offset)?;
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
            Token::Word(w) if matches!(w.as_str(), "if" | "while") => *w,
            Token::Word(w) if matches!(w.as_str(), "unless" | "until") && !canonical() => *w,
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
            Box::pin(self.reject_modifier()).await?;
            return Ok(stmt);
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
            let p = self.p();
            p.work.charge(1)?;
            if !p.statement_continues()? {
                return Ok(stmt);
            }
            let stmt = stmt.at(offset);
            let depth = stmt.depth;
            p.make_at(Node::Compound(Boxed::new(p.work, stmt)?), depth, offset)?
        };
        Ok(Statement::Expr(self.statement_tail(expr).await?))
    }

    // Go parses a statement-position begin as a statement, which costs no
    // expression nesting, and then continues any expression after its end.
    async fn begin_statement(&self) -> Result<Statement> {
        let offset = {
            let mut p = self.p();
            let offset = p.tokens[p.pos].offset as u32;
            p.pos += 1;
            offset
        };
        let mut attempt = self.begin_expression(offset).await?;
        attempt.offset = offset;
        if !self.p().statement_continues()? {
            return Ok(Statement::Expr(attempt));
        }
        Ok(Statement::Expr(self.statement_tail(attempt).await?))
    }

    /// Continues a compound statement as an expression. Go reads this
    /// continuation without its line limit, and the operands inside it too
    /// when no enclosing line expression limits them.
    async fn statement_tail(&self, expr: Expr) -> Result<Expr> {
        let open = {
            let mut p = self.p();
            let open = p.line_exprs == 0;
            if open {
                p.groups += 1;
            }
            open
        };
        let result = self.expr_tail(expr, 0, None, None, true).await;
        if open {
            self.p().groups -= 1;
        }
        result
    }

    async fn plain_statement(&self) -> Result<Statement> {
        let keyword = {
            let mut p = self.p();
            p.work.charge(1)?;
            let keyword = [
                "raise", "retry", "if", "unless", "while", "until", "for", "return", "break",
                "next",
            ]
            .into_iter()
            .find(|keyword| matches!(p.token(), Token::Word(w) if w == keyword))
            .filter(|keyword| !matches!(*keyword, "unless" | "until") || !canonical());
            if keyword.is_some() {
                p.pos += 1;
            }
            keyword
        };
        match keyword {
            Some("raise") => self.raise_statement().await,
            Some("retry") => {
                let mut p = self.p();
                let line = p.tokens[p.pos - 1].line;
                if !p.ends_after(p.pos - 1, line) && !p.modifier_follows(line) {
                    p.pos = p.significant(p.pos);
                    return p.err("retry does not accept a value");
                }
                Ok(Statement::Retry)
            }
            Some(keyword @ ("if" | "unless")) => self.if_stmt(keyword == "unless").await,
            Some(keyword @ ("while" | "until")) => self.while_stmt(keyword == "until").await,
            Some("for") => self.for_stmt().await,
            Some(flow) => self.flow_statement(flow).await,
            None if self.p().typed_local_ahead()? => self.typed_local().await,
            None if self.p().assertion() => self.assertion().await,
            None if self.p().token() == &Token::Op("*") || self.p().assignment_ahead()? => {
                self.assignment_statement().await
            }
            None => {
                let start = self.p().pos;
                let expr = self.line_expr(0).await?;
                // Go reads an expression followed by a comma as a destructuring
                // target list, and one followed by an assignment operator as a target.
                let target = {
                    let p = self.p();
                    match &p.tokens[p.significant(p.pos)].token {
                        Token::P(',') => true,
                        Token::Op(op) => assignment(op),
                        _ => false,
                    }
                };
                if target {
                    self.p().pos = start;
                    return self.assignment_statement().await;
                }
                Ok(Statement::Expr(self.trailing_block(expr).await?))
            }
        }
    }

    /// Parses `assert` and its arguments, as Go's `parseAssertStatement` does.
    async fn assertion(&self) -> Result<Statement> {
        let work = self.p().work;
        let (callee, offset) = {
            let mut p = self.p();
            let offset = p.tokens[p.pos].offset as u32;
            let line = p.tokens[p.pos].line;
            p.bump()?;
            let callee = p.variable_name("assert")?;
            if p.ends_after(p.pos - 1, line) {
                return Ok(Statement::Expr(callee));
            }
            p.line_breaks()?;
            (callee, offset)
        };
        let mut args = Buffer::new();
        loop {
            let value = self.line_expr(0).await?;
            args.push(
                work,
                Argument {
                    kind: ArgumentKind::Positional,
                    value,
                },
            )?;
            let mut p = self.p();
            if !p.comma_follows() {
                break;
            }
            p.line_breaks()?;
        }
        let depth =
            1 + call_depth(&callee).max(args.iter().map(|a| a.value.depth).max().unwrap_or(0));
        let call = Node::Call(Name::new(work, "assert")?, args, CallForm::Bare);
        Ok(Statement::Expr(self.p().make_at(call, depth, offset)?))
    }

    async fn flow_statement(&self, flow: &str) -> Result<Statement> {
        let value = {
            let mut p = self.p();
            let line = p.tokens[p.pos - 1].line;
            let value = !p.ends_after(p.pos - 1, line) && !p.modifier_follows(line);
            if value {
                p.line_breaks()?;
            }
            value
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
        let (target, _) = self.target(Place::Statement).await?;
        let op = {
            let mut p = self.p();
            let next = p.significant(p.pos);
            if matches!(&p.tokens[next].token, Token::Op(op) if assignment(op)) {
                p.pos = next;
            }
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
                    p.assignment_member(expr)?;
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
            return Ok(Statement::Expr(self.block_line_expr().await?));
        };
        let first = self.block_line_expr().await?;
        let rhs = if matches!(target, Target::Tuple(_)) && self.p().comma_follows() {
            let work = self.p().work;
            let mut items = Buffer::from_array(work, [first])?;
            loop {
                {
                    // Like Go, a comma the line ends on closes the list.
                    let mut p = self.p();
                    let comma = p.pos - 1;
                    if p.ends_after(comma, p.tokens[comma].line) {
                        break;
                    }
                    p.line_breaks()?;
                }
                items.push(work, self.block_line_expr().await?)?;
                if !self.p().comma_follows() {
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
            p.line_breaks()?;
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
        let (target, _) = self.target(Place::For).await?;
        let previous = {
            let mut p = self.p();
            if !target.is_binding() {
                let offset = target
                    .offset()
                    .map_or(p.position(p.pos), |offset| offset as usize);
                return Err(Error::syntax(p.work, offset, "invalid for loop target"));
            }
            p.line_breaks()?;
            p.expect_word("in")?;
            p.line_breaks()?;
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

    /// Parses a destructuring target list, as Go's `parseDestructureTargetList`
    /// does, and reports whether commas at its own level made it a tuple.
    async fn target(&self, place: Place) -> Result<(Target, bool)> {
        let work = self.p().work;
        work.charge(1)?;
        let typed = place == Place::Group(true);
        let mut parts = Buffer::new();
        let mut tuple = false;
        let mut has_rest = false;
        loop {
            let (rest, anonymous, group, star) = {
                let mut p = self.p();
                let star = p.tokens[p.pos].offset as u32;
                let rest = p.token() == &Token::Op("*");
                let mut anonymous = false;
                if rest {
                    let next = p.significant(p.pos + 1);
                    anonymous = match &p.tokens[next].token {
                        Token::P(',' | ')' | ']') => true,
                        Token::Op(op) => assignment(op),
                        Token::Word(w) => place == Place::For && *w == "in",
                        _ => false,
                    };
                    p.bump()?;
                    if !anonymous {
                        p.line_breaks()?;
                    }
                    tuple = true;
                }
                // Go reads a statement's first target as an expression, so it
                // cannot open a group.
                let grouped = place != Place::Statement || !parts.is_empty() || rest;
                let group = match p.token() {
                    Token::P('(') if grouped && !anonymous => Some(')'),
                    Token::P('[') if grouped && !anonymous => Some(']'),
                    _ => None,
                };
                (rest, anonymous, group, star)
            };
            let (mut value, offset) = if anonymous {
                (None, star)
            } else if let Some(close) = group {
                // Go counts each nested destructuring group against the syntax limit.
                let open = {
                    let mut p = self.p();
                    p.enter()?;
                    let open = p.tokens[p.pos].offset as u32;
                    p.bump()?;
                    let next = p.significant(p.pos);
                    if p.tokens[next].token == Token::P(close) {
                        p.pos = next;
                        return p.expected(Label::Text("destructuring assignment target"));
                    }
                    p.line_breaks()?;
                    open
                };
                let (inner, tuple) = self.nested_target(typed).await?;
                let mut p = self.p();
                p.line_breaks()?;
                p.expect_p(close)?;
                p.depth -= 1;
                // Like Go, every group destructures one level; commas inside
                // it list that level's parts.
                let inner = if tuple {
                    inner
                } else {
                    Target::Tuple(Buffer::from_array(work, [(Some(inner), false)])?)
                };
                (Some(inner), open)
            } else {
                let expression = self.line_expr(0).await?;
                let p = self.p();
                // Go checks a statement's lone target only once an operator follows.
                let lone = place == Place::Statement && parts.is_empty() && !rest;
                let listed = p.tokens[p.significant(p.pos)].token == Token::P(',');
                if !lone || listed {
                    p.assignment_member(&expression)?;
                    if let Some(offset) = expression.safe_navigation() {
                        return Err(Error::syntax(
                            p.work,
                            offset,
                            "safe navigation cannot be used as an assignment target",
                        ));
                    }
                    if !expression.assignable() {
                        return Err(Error::syntax(
                            p.work,
                            expression.offset as usize,
                            "invalid destructuring assignment target",
                        ));
                    }
                }
                let offset = expression.offset;
                (Some(Target::Value(expression)), offset)
            };
            let mut p = self.p();
            let colon = p.significant(p.pos);
            if typed && value.is_some() && p.tokens[colon].token == Token::P(':') {
                p.pos = colon + 1;
                p.line_breaks()?;
                let start = p.tokens[p.pos].offset;
                let ty = p.type_expr(1, false)?;
                if rest && !ty.captures(false) {
                    let mut text = Vec::new();
                    target_text(value.as_ref().unwrap(), &mut text)?;
                    work.bytes(text.len())?;
                    let text = String::from_utf8_lossy(&text);
                    return Err(Error::syntax(
                        work,
                        start,
                        format_args!(
                            "rest destructuring target {} captures an array; annotate it as array<...> or any",
                            source_text(if text.is_empty() { "*" } else { &text })
                        ),
                    ));
                }
                value = Some(Target::Typed(Boxed::new(work, value.take().unwrap())?, ty));
            }
            if rest {
                if has_rest {
                    return Err(Error::syntax(
                        work,
                        offset as usize,
                        "duplicate rest assignment target",
                    ));
                }
                has_rest = true;
            }
            parts.push(work, (value, rest))?;
            let comma = p.significant(p.pos);
            if p.tokens[comma].token != Token::P(',') {
                break;
            }
            p.pos = comma + 1;
            tuple = true;
            p.line_breaks()?;
        }
        let target = if tuple {
            Target::Tuple(parts)
        } else {
            parts.pop().unwrap().0.unwrap()
        };
        let p = self.p();
        let offset = target.offset().unwrap_or(p.tokens[p.pos].offset as u32);
        p.check_depth(target.depth(), offset)?;
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
            let yes = self.trailing_block(yes).await?;
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
            let no = self.trailing_block(no).await?;
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
                // Like Go, a comma on the next line continues the values.
                let mut p = self.p();
                let comma = p.significant(p.pos);
                if p.tokens[comma].token != Token::P(',') {
                    break;
                }
                p.pos = comma + 1;
                p.lines()?;
            }
            {
                let mut p = self.p();
                p.word("then");
                p.lines()?;
            }
            let result = self.expr(0).await?;
            let result = self.trailing_block(result).await?;
            clauses.push(work, When { values, result })?;
            self.p().lines()?;
        }
        if clauses.is_empty() {
            return self.p().expected(Label::Text("when"));
        }
        let alternate = if self.p().word("else") {
            self.p().lines()?;
            let alternate = self.expr(0).await?;
            Some(Boxed::new(work, self.trailing_block(alternate).await?)?)
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
        let line = {
            let p = self.p();
            p.tokens[p.pos].line
        };
        let lhs = self.prefix().await?;
        let result = self.expr_tail(lhs, min, None, Some(line), false).await;
        self.p().depth -= 1;
        result
    }

    async fn prefix(&self) -> Result<Expr> {
        let (offset, grouped) = {
            let p = self.p();
            p.work.charge(1)?;
            // Go locates a folded negative number at its digits.
            let folded = p.token() == &Token::Op("-") && p.negative_literal(p.pos)?;
            (
                p.tokens[p.pos + usize::from(folded)].offset as u32,
                p.token() == &Token::P('('),
            )
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
            p.line_breaks()?;
        }
        let e = self.expr(0).await?;
        let mut p = self.p();
        p.line_breaks()?;
        p.expect_p(')')?;
        p.groups -= 1;
        Ok(e)
    }

    async fn array_expression(&self) -> Result<Expr> {
        let a = self.arguments(']', true).await?;
        let d = 1 + a.iter().map(|e| e.depth).max().unwrap_or(0);
        self.p().make(Node::Array(a), d)
    }

    async fn open_range_expression(&self, op: &str) -> Result<Expr> {
        {
            let mut p = self.p();
            let next = p.significant(p.pos);
            if !p.prefix(next) {
                p.pos -= 1;
                return p.err("range is missing end expression");
            }
            p.pos = next;
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
            "if" => self.if_expr(false).await,
            "unless" if !canonical() => self.if_expr(true).await,
            "case" => self.case_expr().await,
            "yield" => self.yield_expr().await,
            "begin" => self.begin_expression(offset).await,
            "while" | "for" => self.loop_expression(w, offset).await,
            "until" if !canonical() => self.loop_expression(w, offset).await,
            // Like Go, only `self` and `then` among the remaining keywords
            // start an expression.
            _ if keyword(w) && !matches!(w, "self" | "then") => Err(Error::syntax(
                self.p().work,
                offset as usize,
                format_args!("unexpected token {}", Label::word(w)),
            )),
            _ => self.p().variable_name(w),
        }
    }

    async fn begin_expression(&self, offset: u32) -> Result<Expr> {
        let body = self.block(&["rescue", "else", "ensure", "end"]).await?;
        let attempt = self.rescue_tail(body, false, offset).await?;
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
            op == "-" && p.negative_literal(p.pos - 1)?
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
                match p.token() {
                    Token::P(',') => (),
                    Token::Eof => return p.expected(Label::Char('}')),
                    _ => return p.err(INVALID_HASH_PAIR),
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
        self.p().expression_separator()?;
        Ok(expr)
    }

    /// Applies suffixes to `lhs`, starting with `next` if given. `line` is the
    /// line a prefix spanning lines started on, which Go keeps as the limit
    /// for the first suffix; later suffixes are limited to the line their
    /// predecessor's last token starts on. An `unlimited` tail takes suffixes
    /// from any line, as Go's `continueExpressionParse` does without a limit.
    async fn expr_tail(
        &self,
        mut lhs: Expr,
        min: u8,
        mut next: Option<Suffix>,
        mut line: Option<usize>,
        unlimited: bool,
    ) -> Result<Expr> {
        self.p().work.charge(1)?;
        loop {
            let suffix = match next.take() {
                Some(suffix) => Some(suffix),
                None if unlimited => self.p().unlimited_suffix(&lhs, min)?,
                None => self.p().expression_suffix(&lhs, min, line.take())?,
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
            let p = self.p();
            line = Some(p.tokens[p.previous_index()?].line);
        }
    }

    async fn index_expression(&self, lhs: Expr, offset: u32) -> Result<Expr> {
        {
            let mut p = self.p();
            let close = p.significant(p.pos);
            if p.tokens[close].token == Token::P(']') {
                p.pos = close;
                return p.err("index expression requires at least one selector");
            }
        }
        let indexes = self.arguments(']', false).await?;
        let p = self.p();
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
            if matches!(p.token(), Token::Op("/" | "//")) {
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
        p.suffix_call(&lhs)?;
        let node = match lhs.into_node() {
            Node::Var(name) => Node::Call(name, args, CallForm::Bare),
            Node::Member(receiver, name) => Node::Method(receiver, name, args, CallForm::Bare),
            Node::SafeMember(receiver, name) => {
                Node::SafeMethod(receiver, name, args, CallForm::Bare)
            }
            Node::Scope(receiver, name, None) => Node::Scope(receiver, name, Some(args)),
            _ => unreachable!(),
        };
        p.make_at(node, depth, offset)
    }

    async fn block_expression(&self, mut lhs: Expr, brace: bool) -> Result<Expr> {
        self.p().work.charge(1)?;
        if brace {
            Box::pin(self.hash_block(&lhs)).await?;
        }
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
        let mut p = self.p();
        p.suffix_call(&lhs)?;
        p.make_at(
            Node::BlockCall(Boxed::new(p.work, lhs)?, block),
            depth,
            offset,
        )
    }

    /// Refuses braces after a call that hold a hash literal, `puts { a: 1 }`,
    /// rather than a block's statements. A `{` after a call starts its
    /// block, so a hash argument needs the call's parentheses; the fix adds
    /// them when the call has no other arguments.
    async fn hash_block(&self, lhs: &Expr) -> Result<()> {
        // Only a call without arguments takes the hash as its first.
        let (name, bare) = match &lhs.node {
            Node::Var(name) | Node::Member(_, name) | Node::SafeMember(_, name) => (name, true),
            Node::Call(name, ..) | Node::Method(_, name, ..) | Node::SafeMethod(_, name, ..) => {
                (name, false)
            }
            _ => return Ok(()),
        };
        let open = {
            let mut p = self.p();
            let open = p.pos;
            let first = p.significant(open + 1);
            let key = match &p.tokens[first].token {
                Token::Word(word) if !word.starts_with('@') => {
                    p.pos = first;
                    let typed = p.typed_local_ahead();
                    p.pos = open;
                    !typed?
                }
                Token::Bytes(_) | Token::QuotedSymbol(_) => true,
                _ => false,
            };
            // A key is never the last token, which is the end of input.
            if !key
                || p.tokens[first + 1].token != Token::P(':')
                || p.tokens[first + 1].offset != p.tokens[first].end
            {
                return Ok(());
            }
            open
        };
        // Read the braces as the hash they hold, to find where it ends.
        self.p().pos = open + 1;
        let end = match self.hash_expr().await {
            Ok(_) => Some(self.p().pos),
            Err(error) if error.kind == crate::ErrorKind::Syntax => None,
            Err(error) => return Err(error),
        };
        let p = self.p();
        let brace = p.tokens[open].offset;
        let message = format!(
            "a hash literal passed to `{name}` needs parentheses, as in `{name}({{ ... }})`; after a call, `{{` starts a block"
        );
        let close = end.map(|end| p.tokens[end - 1].end);
        let span = crate::diagnostic::Span::new(brace, close.unwrap_or(brace + 1));
        let mut diagnostic = crate::diagnostic::Diagnostic::error(
            crate::diagnostic::Code::HASH_ARGUMENT,
            span,
            message.clone(),
        );
        let ends_call = |index: usize| match &p.tokens[index].token {
            Token::EndLine | Token::Eof | Token::P('}' | ')' | ']') => true,
            Token::Word(word) => matches!(word.as_str(), "if" | "unless" | "while" | "until"),
            _ => false,
        };
        if let (Some(end), Some(close), true) = (end, close, bare)
            && ends_call(end)
        {
            diagnostic = diagnostic.with_fix(crate::diagnostic::Fix::edits(
                "pass the hash in parentheses",
                vec![
                    crate::diagnostic::Edit {
                        span: crate::diagnostic::Span::new(p.tokens[open - 1].end, brace),
                        replacement: "(".to_owned(),
                    },
                    crate::diagnostic::Edit {
                        span: crate::diagnostic::Span::at(close),
                        replacement: ")".to_owned(),
                    },
                ],
            ));
        }
        Err(Error::syntax(p.work, brace, message).with_diagnostic(diagnostic))
    }

    async fn scoped_expression(&self, lhs: Expr) -> Result<Expr> {
        let work = self.p().work;
        work.charge(1)?;
        let offset = lhs.offset;
        let (name, parenthesized) = {
            let mut p = self.p();
            p.bump()?;
            p.line_breaks()?;
            if !p.ident(p.pos) && !matches!(p.token(), Token::Word(w) if w == "enum") {
                return p.expected(Label::Text("identifier"));
            }
            let Token::Word(name) = p.bump()? else {
                unreachable!()
            };
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
            self.p().type_call = name == "as"
                || (name == "parse_as"
                    && matches!(&lhs.node, Node::Var(receiver) if receiver == "JSON"));
            let mut args = self.call_arguments().await?;
            if name == "as" {
                self.p().nil_type_argument(&mut args)?;
            }
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
        let _scope = self.recovery_scope()?;
        let (offset, outer, outer_it) = {
            let mut p = self.p();
            p.work.charge(1)?;
            let offset = p.tokens[p.pos].offset as u32;
            p.bump()?;
            p.line_breaks()?;
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
            if !p.take_p('}') {
                return p.expected(Label::Text("}"));
            }
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

    /// Parses a block's parameter list, as Go's `parseBlockParameters` does.
    async fn block_parameters(&self) -> Result<(Buffer<Target>, bool)> {
        let work = self.p().work;
        let mut params = Buffer::new();
        let explicit = if self.p().token() == &Token::Op("||") {
            self.p().bump()?;
            true
        } else if self.p().take_p('|') {
            self.p().line_breaks()?;
            if !self.p().take_p('|') {
                loop {
                    let target = self.block_parameter().await?;
                    let mut p = self.p();
                    p.declare_target(&target)?;
                    params.push(work, target)?;
                    let comma = p.significant(p.pos);
                    if p.tokens[comma].token != Token::P(',') {
                        break;
                    }
                    p.pos = comma + 1;
                    p.line_breaks()?;
                    if p.token() == &Token::P('|') {
                        return p.err("trailing comma in block parameter list");
                    }
                }
                let mut p = self.p();
                p.line_breaks()?;
                if !p.take_p('|') {
                    return p.expected(Label::Char('|'));
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

    /// Parses one block parameter, as Go's `parseBlockParameter` does.
    async fn block_parameter(&self) -> Result<Target> {
        let work = self.p().work;
        let (close, open) = {
            let mut p = self.p();
            let open = p.tokens[p.pos].offset;
            let close = match p.token() {
                Token::P('(') => ')',
                Token::P('[') => ']',
                _ if p.ident(p.pos) => {
                    let name = p.name()?;
                    let target = Target::Value(p.make(Node::Var(name), 1)?);
                    let colon = p.significant(p.pos);
                    if p.tokens[colon].token != Token::P(':') {
                        return Ok(target);
                    }
                    p.pos = colon + 1;
                    p.line_breaks()?;
                    return Ok(Target::Typed(
                        Boxed::new(work, target)?,
                        p.type_expr(0, true)?,
                    ));
                }
                _ => return p.expected(Label::Text("block parameter")),
            };
            p.enter()?;
            p.bump()?;
            let next = p.significant(p.pos);
            if p.tokens[next].token == Token::P(close) {
                p.pos = next;
                return p.expected(Label::Text("destructuring assignment target"));
            }
            p.line_breaks()?;
            (close, open)
        };
        let (target, tuple) = self.nested_target(true).await?;
        let mut p = self.p();
        p.line_breaks()?;
        p.expect_p(close)?;
        p.depth -= 1;
        let target = if tuple {
            target
        } else {
            Target::Tuple(Buffer::from_array(work, [(Some(target), false)])?)
        };
        if !target.is_binding() {
            return Err(Error::syntax(
                work,
                open,
                "invalid block parameter destructuring target",
            ));
        }
        Ok(target)
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
            self.arguments(')', true).await?
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
        let start = self.p().pos;
        let mut hash = None;
        loop {
            if keywords {
                self.p().keyword_order(
                    "positional arguments cannot follow bare keyword arguments in parenless calls",
                )?;
            }
            let argument = self.call_argument(false, false).await?;
            let mut p = self.p();
            keywords |= matches!(
                argument.kind,
                ArgumentKind::Keyword(_) | ArgumentKind::KeywordSplat
            );
            args.push(work, argument)?;
            let last = p.previous()?;
            if p.token() != &Token::P(',')
                || p.tokens[p.pos].line != last.line
                || p.tokens[p.pos + 1].line != last.line
            {
                break;
            }
            // Read a hash literal after a comma, to refuse it with a fix.
            let brace = p.tokens[p.pos + 1].token == Token::P('{');
            if !brace && !p.command_argument_start(p.pos + 1, true) {
                break;
            }
            if brace {
                hash.get_or_insert(p.tokens[p.pos + 1].offset);
            }
            p.bump()?;
        }
        if let Some(brace) = hash {
            return Err(self.p().hash_argument(start, brace));
        }
        Ok(args)
    }

    /// Parses a comma-separated list through `close`, as Go reads array
    /// elements, or index selectors when `trailing` refuses a trailing comma.
    async fn arguments(&self, close: char, trailing: bool) -> Result<Buffer<Expr>> {
        let work = self.p().work;
        work.charge(1)?;
        let mut args = Buffer::new();
        {
            let mut p = self.p();
            p.groups += 1;
            p.line_breaks()?;
            if p.take_p(close) {
                p.groups -= 1;
                return Ok(args);
            }
        }
        loop {
            args.push(work, self.expr(0).await?)?;
            let mut p = self.p();
            p.line_breaks()?;
            if p.take_p(close) {
                break;
            }
            if p.token() != &Token::P(',') {
                return p.expected(Label::Char(close));
            }
            p.expect_p(',')?;
            p.line_breaks()?;
            if trailing && p.take_p(close) {
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
        let types = std::mem::take(&mut self.p().type_call);
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
            if keywords {
                self.p()
                    .keyword_order("positional arguments cannot follow keyword arguments")?;
            }
            let argument = self.call_argument(true, types).await?;
            let mut p = self.p();
            keywords |= matches!(
                argument.kind,
                ArgumentKind::Keyword(_) | ArgumentKind::KeywordSplat
            );
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
        let mut p = self.p();
        p.groups -= 1;
        p.call_end = p.pos;
        Ok(args)
    }

    /// Parses one argument; one of a call that takes `types` may be a tuple
    /// type.
    async fn call_argument(&self, parenthesized: bool, types: bool) -> Result<Argument> {
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
            let literal = p.literal_argument(&kind, parenthesized, types)?;
            p.type_argument = types
                && parenthesized
                && matches!(kind, ArgumentKind::Positional)
                && p.token() == &Token::P('{');
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
            Token::Symbol(_) | Token::QuotedSymbol(_) => Label::Text("symbol"),
            Token::Invalid(invalid) => match invalid.failure {
                Failure::Literal(label) => Label::Text(label),
                Failure::Diagnostic | Failure::Character => Label::Text("invalid token"),
            },
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
        let token = &self.tokens[index];
        if token.token == Token::P('?')
            && self.source.as_bytes().get(token.end) == Some(&b'=')
            && index.checked_sub(1).is_some_and(|previous| {
                let name = &self.tokens[previous];
                name.end == token.offset
                    && matches!(&name.token, Token::Word(word) if !keyword(word)
                    || previous.checked_sub(1).is_some_and(|separator| {
                        matches!(self.tokens[separator].token, Token::P('.') | Token::Op("&."))
                    }))
            })
        {
            return Some(self.name_suffix_error(token.offset));
        }
        match &self.tokens[index].token {
            Token::Invalid(invalid) if invalid.failure == Failure::Diagnostic => Some(
                Error::syntax(self.work, invalid.offset, invalid.message.as_str()),
            ),
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
    fn expression_separator(&self) -> Result<()> {
        let previous = &self.tokens[self.pos - 1];
        let next = &self.tokens[self.pos];
        if previous.token != Token::EndLine
            && previous.end_line == next.line
            && self.prefix(self.pos)
            && !matches!(&next.token, Token::Word(word) if matches!(word.as_str(), "unless" | "until" | "do"))
        {
            let message = ADJACENT_EXPRESSIONS;
            let diagnostic = crate::diagnostic::Diagnostic::error(
                crate::diagnostic::Code::SYNTAX,
                crate::diagnostic::Span::new(previous.end, next.offset),
                message,
            );
            return Err(Error::syntax(self.work, previous.end, message).with_diagnostic(diagnostic));
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
    /// Go's test that a token has a prefix parser: whether the token at
    /// `index` can start an expression.
    fn prefix(&self, index: usize) -> bool {
        match &self.tokens[index].token {
            Token::Word(w) if w.starts_with('@') => true,
            Token::Word(w) if keyword(w) => matches!(
                w.as_str(),
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
            ),
            Token::Word(_)
            | Token::Int(_)
            | Token::BigInt(..)
            | Token::Float(_)
            | Token::Bytes(_)
            | Token::Template(_)
            | Token::Words(_)
            | Token::Regex(..)
            | Token::Symbol(_)
            | Token::QuotedSymbol(_)
            | Token::P('(' | '[' | '{')
            | Token::Op("!" | "-" | "+" | "->") => true,
            Token::Invalid(invalid) => matches!(invalid.failure, Failure::Literal(_)),
            _ => false,
        }
    }
    /// Reports whether the token at `index` is an identifier, as opposed to a
    /// keyword or variable.
    fn ident(&self, index: usize) -> bool {
        matches!(&self.tokens[index].token, Token::Word(w) if !keyword(w) && !w.starts_with('@'))
    }
    /// Consumes a symbol token and returns its name.
    fn symbol_name(&mut self) -> Result<Option<Name>> {
        let name = match self.token() {
            Token::Symbol(name) => Name::new(self.work, name)?,
            Token::QuotedSymbol(bytes) => {
                self.work.bytes(bytes.len())?;
                let name = std::str::from_utf8(bytes)
                    .map_err(|_| unsupported(self.work, "method names must be UTF-8"))?;
                Name::new(self.work, name)?
            }
            _ => return Ok(None),
        };
        self.work.checkpoint()?;
        self.work.bytes(name.len())?;
        if let Err(error) = method_spelling(&name, false) {
            let token = &self.tokens[self.pos];
            if matches!(token.token, Token::QuotedSymbol(_)) {
                use crate::diagnostic::{Code, Diagnostic, Fix, Span};
                let message = error.message();
                let span = Span::new(token.offset, token.end);
                let mut diagnostic = Diagnostic::error(Code::SYNTAX, span, &message);
                if let MethodNameError::Suffix(suffix) = error {
                    diagnostic = Diagnostic::error(Code::NAME_SUFFIX, span, &message);
                    // As in source, a `?` or `!` inside the name has no repair.
                    if name[suffix..].bytes().all(|b| matches!(b, b'?' | b'!')) {
                        let mut fixed = name.to_string();
                        fixed.remove(suffix);
                        diagnostic = diagnostic.with_fix(Fix::replace(
                            "remove the name suffix",
                            span,
                            format!(":{fixed:?}"),
                        ));
                    }
                }
                return Err(
                    Error::syntax(self.work, token.offset, message).with_diagnostic(diagnostic)
                );
            }
            return Err(error.diagnostic(self.work, self.source, token.offset + 1));
        }
        self.bump()?;
        Ok(Some(name))
    }
    fn name(&mut self) -> Result<Name> {
        self.read_name(false)
    }
    fn method_name(&mut self) -> Result<Name> {
        self.read_name(true)
    }
    fn read_name(&mut self, method: bool) -> Result<Name> {
        self.work.charge(1)?;
        let offset = self.tokens[self.pos].offset;
        if let Token::Word(w) = self.bump()? {
            if method {
                self.method_spelling(&w, offset)?;
            } else {
                self.binding_name(&w, offset)?;
            }
            if reserved(&w) {
                return Err(Error::syntax(self.work, offset, "reserved name"));
            }
            Name::new(self.work, &w)
        } else {
            Err(Error::syntax(self.work, offset, "expected name"))
        }
    }
    fn binding_name(&self, name: &str, offset: usize) -> Result<()> {
        if let Some(suffix) = name_suffix_position(name) {
            return Err(self.name_suffix_error(offset + suffix));
        }
        Ok(())
    }
    fn method_spelling(&self, name: &str, offset: usize) -> Result<()> {
        self.work.checkpoint()?;
        self.work.bytes(name.len())?;
        method_spelling(name, true)
            .map_err(|error| error.diagnostic(self.work, self.source, offset))
    }
    fn name_suffix_error(&self, offset: usize) -> Error {
        name_suffix_error(self.work, self.source, offset)
    }
    /// Records the bare read of `name`, just consumed, when its name without
    /// the suffix, spelled as V0003 repairs a declaration, is bound here.
    fn suffix_read(&mut self, name: &str) -> Result<()> {
        let Some(stem) = name.strip_suffix(['?', '!']) else {
            return Ok(());
        };
        // A keyword is short, so its repaired spelling needs no accounting.
        let bound = if keyword(stem) {
            self.locals.contains(self.work, &format!("{stem}_"))?
        } else {
            self.locals.contains(self.work, stem)?
        };
        if bound {
            let token = &self.tokens[self.pos - 1];
            let span = (token.offset as u32, token.end as u32);
            self.suffix_reads.push(self.work, span)?;
        }
        Ok(())
    }
    /// Drops the read recorded at `offset` once it turns out to be a call,
    /// which a local cannot answer.
    fn suffix_call(&mut self, callee: &Expr) -> Result<()> {
        if matches!(&callee.node, Node::Var(name) if name.ends_with(['?', '!'])) {
            for index in (0..self.suffix_reads.len()).rev() {
                self.work.charge(1)?;
                if self.suffix_reads[index].0 == callee.offset {
                    self.suffix_reads.remove(index);
                    break;
                }
            }
        }
        Ok(())
    }
    fn assignment_member(&self, expr: &Expr) -> Result<()> {
        if matches!(&expr.node, Node::Member(_, name) | Node::SafeMember(_, name)
            if name.ends_with(['?', '!']))
        {
            // Member offsets point to the receiver; its name is the last word
            // before any closing parentheses around the target.
            for index in (0..self.pos).rev() {
                self.work.charge(1)?;
                let token = &self.tokens[index];
                if matches!(token.token, Token::Word(_)) {
                    return Err(self.name_suffix_error(token.end - 1));
                }
            }
        }
        Ok(())
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
    /// Refuses a syntax tree deeper than Go's limit, at the offending node as Go does.
    fn check_depth(&self, depth: u32, offset: u32) -> Result<()> {
        if depth as usize > MAX_DEPTH {
            self.work.charge(1)?;
            Err(Error::syntax(self.work, offset as usize, TOO_DEEP))
        } else {
            Ok(())
        }
    }
    fn declare_target(&mut self, target: &Target) -> Result<()> {
        self.work.charge(1)?;
        let mut names = Buffer::new();
        let mut invalid = None;
        target.parts(|part, _| {
            if let Target::Value(Expr {
                node: Node::Var(name),
                offset,
                ..
            }) = part
            {
                if let Err(error) = self.binding_name(name, *offset as usize) {
                    invalid = Some(error);
                    return false;
                }
                names.push(self.work, name.clone()).is_ok()
            } else {
                true
            }
        });
        if let Some(error) = invalid {
            return Err(error);
        }
        for name in names {
            self.work.charge(1)?;
            self.declared_it |= name == "it";
            self.locals.insert(self.work, name, ())?;
        }
        Ok(())
    }
    // Like Go, the separator may start a later line.
    fn ternary_separator(&mut self) -> Result<()> {
        let mut next = self.pos;
        while self.tokens[next].token == Token::EndLine
            && self.tokens[next].line != self.tokens[next].end_line
        {
            self.work.charge(1)?;
            next += 1;
        }
        if self.tokens[next].token == Token::P(':') {
            self.pos = next;
        }
        Ok(())
    }
    /// Consumes a comma that follows, skipping line breaks as Go's lookahead does.
    fn comma_follows(&mut self) -> bool {
        let comma = self.significant(self.pos);
        if self.tokens[comma].token != Token::P(',') {
            return false;
        }
        self.pos = comma + 1;
        true
    }
    /// Reports whether `assert` starts Go's assertion statement, which takes a
    /// comma-separated argument list unless parentheses follow directly.
    fn assertion(&self) -> bool {
        matches!(self.token(), Token::Word(w) if *w == "assert")
            && !(self.tokens[self.pos + 1].token == Token::P('(')
                && self.tokens[self.pos + 1].offset == self.tokens[self.pos].end)
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
        let offset = self.tokens[self.pos.saturating_sub(1)].offset as u32;
        self.make_at(node, depth, offset)
    }
    fn make_at(&self, node: Node, depth: u32, offset: u32) -> Result<Expr> {
        self.work.charge(1)?;
        self.check_depth(depth, offset)?;
        Ok(Expr {
            node,
            depth,
            offset,
        })
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
            | Token::Symbol(_)
            | Token::QuotedSymbol(_) => true,
            Token::Word(w) => {
                !keyword(w) || matches!(w.as_str(), "nil" | "true" | "false" | "self" | "then")
            }
            _ => false,
        };
        if !leaf {
            return Ok(Leaf::Nested);
        }
        self.work.charge(1)?;
        self.enter()?;
        self.work.charge(1)?;
        let offset = self.tokens[self.pos].offset as u32;
        let line = self.tokens[self.pos].line;
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
        Ok(match self.expression_suffix(&lhs, min, Some(line))? {
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
            Token::Template(parts) => {
                let offset = self.tokens[self.pos - 1].offset;
                self.template(parts, false, offset)
            }
            Token::Words(words) => {
                let offset = self.tokens[self.pos - 1].offset;
                // Go lexes a `%` after an operand as modulo and reads it again
                // as a percent literal only for a command argument.
                if words.ambiguous && self.pos - 1 != self.percent_argument {
                    return Err(Error::syntax(self.work, offset, "unexpected token \"%\""));
                }
                self.words(words.into_inner(), offset)
            }
            Token::Symbol(name) => {
                let name = Bytes::from_slice(self.work, name.as_bytes())?;
                self.make(Node::Literal(name.into_value(true)), 1)
            }
            Token::QuotedSymbol(name) => self.make(Node::Literal(name.into_value(true)), 1),
            Token::Invalid(invalid) if invalid.failure == Failure::Character => {
                self.unexpected(self.pos - 1)
            }
            Token::Invalid(invalid) => Err(Error::syntax(
                self.work,
                invalid.offset,
                invalid.message.as_str(),
            )),
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
    /// Reads the name just consumed. Like Go, a variable sigil at the end of
    /// input names nothing.
    fn variable_name(&mut self, name: &str) -> Result<Expr> {
        if name.starts_with('@') {
            self.binding_name(name, self.tokens[self.pos - 1].offset)?;
        } else {
            self.method_spelling(name, self.tokens[self.pos - 1].offset)?;
            self.suffix_read(name)?;
        }
        if matches!(name, "@" | "@@") {
            let (expected, got) = if name == "@" {
                ("instance variable name", "instance variable")
            } else {
                ("class variable name", "class variable")
            };
            return Err(Error::syntax(
                self.work,
                self.tokens[self.pos - 1].offset,
                format_args!("expected {expected}, got {got}"),
            ));
        }
        self.block_reference(name, self.tokens[self.pos - 1].offset)?;
        self.make(Node::Var(Name::new(self.work, name)?), 1)
    }
    /// Reports whether the minus at `sign` folds into the adjacent number.
    fn negative_literal(&self, sign: usize) -> Result<bool> {
        // An adjacent minus belongs to the numeric receiver; power keeps the
        // outer sign.
        Ok(self.tokens[sign].end == self.tokens[sign + 1].offset
            && matches!(
                self.tokens[sign + 1].token,
                Token::Int(_) | Token::BigInt(..) | Token::Float(_)
            )
            && !self
                .tokens
                .find(sign + 2..self.tokens.len(), self.work, |next| {
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

    fn words(&mut self, words: lexer::Words<'a>, offset: usize) -> Result<Expr> {
        self.work.charge(1)?;
        let mut values = Buffer::with_capacity(self.work, words.entries.len())?;
        for word in words.entries {
            values.push(self.work, self.template(word, words.symbol, offset)?)?;
        }
        let depth = 1 + values.iter().map(|v| v.depth).max().unwrap_or(0);
        self.make(Node::Array(values), depth)
    }

    /// Builds a string or symbol from its parts. Like Go, an interpolation's
    /// failure is reported at the literal, at `offset`.
    fn template(
        &mut self,
        parts: crate::compilation::Buffer<Part<'a>>,
        symbol: bool,
        offset: usize,
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
                        let text = &self.source[span.0 as usize..span.1 as usize - 1];
                        if text.trim().is_empty() {
                            return Err(Error::syntax(
                                self.work,
                                offset,
                                "empty string interpolation",
                            ));
                        }
                        self.interpolation(tokens, offset)?
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
        offset: usize,
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
            type_call: false,
            type_argument: false,
            type_structural_error: false,
            interpolations: Buffer::new(),
            suffix_reads: Buffer::new(),
            // Go parses interpolations without the member probe.
            record: None,
            inside_class: false,
            nesting: 0,
            call_end: 0,
            percent_argument: 0,
            block_name: self.block_name.clone(),
            type_names: std::mem::take(&mut self.type_names),
            alias_names: std::mem::take(&mut self.alias_names),
            additions: std::mem::take(&mut self.additions),
        };
        while parser.token() == &Token::EndLine
            && parser.tokens[parser.pos].line != parser.tokens[parser.pos].end_line
        {
            self.work.charge(1)?;
            parser.pos += 1;
        }
        // Interpolation depth is bounded by the lexer, so each level can run
        // its own task stack.
        let parsing = Parsing::<recovery::FailFast>::new(parser);
        let result = parsing.run(Call::Interpolation);
        let parser = parsing.parser.into_inner();
        let complete = parser.token() == &Token::Eof;
        self.locals = parser.locals;
        self.declared_it = parser.declared_it;
        self.type_names = parser.type_names;
        self.alias_names = parser.alias_names;
        self.additions = parser.additions;
        self.interpolations
            .extend(self.work, parser.interpolations)?;
        self.suffix_reads.extend(self.work, parser.suffix_reads)?;
        let expr = match result {
            Ok(Parsed::Expr(expr)) => expr,
            Ok(_) => unreachable!(),
            Err(error) if error.kind == crate::ErrorKind::Syntax => {
                return Err(if error.message == ADJACENT_EXPRESSIONS {
                    error
                } else if error.message == TOO_DEEP {
                    Error::syntax(self.work, offset, TOO_DEEP)
                } else {
                    Error::syntax(
                        self.work,
                        offset,
                        format_args!("invalid string interpolation: {}", error.message),
                    )
                });
            }
            Err(error) => return Err(error),
        };
        if !complete {
            return Err(Error::syntax(
                self.work,
                offset,
                "string interpolation must contain a single expression",
            ));
        }
        Ok(expr)
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
        self.suffix_call(&lhs)?;
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
    /// Finds the next suffix as outside any line expression, where Go ignores
    /// line breaks.
    fn unlimited_suffix(&mut self, lhs: &Expr, min: u8) -> Result<Option<Suffix>> {
        let (line_exprs, groups) = (self.line_exprs, self.groups);
        self.line_exprs = 0;
        self.groups += 1;
        let suffix = self.expression_suffix(lhs, min, None);
        (self.line_exprs, self.groups) = (line_exprs, groups);
        suffix
    }
    /// Reports whether a compound statement continues as an expression, as
    /// Go's `continueStatementExpression` does: an operator or other suffix
    /// on the line of its `end`.
    fn statement_continues(&self) -> Result<bool> {
        let next = &self.tokens[self.pos];
        let continues = match &next.token {
            Token::P('.' | '(' | '[' | '?') | Token::Op("&." | "::") => true,
            Token::Op(op) => binding_power(op).is_some(),
            Token::Words(words) => words.ambiguous,
            Token::Word(w) => w == "rescue" || (w == "do" && !canonical()),
            _ => false,
        };
        Ok(continues && next.line == self.previous()?.end_line)
    }
    fn expression_suffix(
        &mut self,
        lhs: &Expr,
        min: u8,
        line: Option<usize>,
    ) -> Result<Option<Suffix>> {
        let resumed = self.continuation_position(min)?;
        if let Some(next) = resumed {
            self.pos = next;
        } else if line.is_some_and(|line| self.tokens[self.pos].line > line)
            && self.line_exprs > 0
            && self.token() != &Token::EndLine
            && !self.limit_continues(self.pos)?
            && !self.begin_call(lhs)
        {
            // Go limits a line expression to the line its prefix started on,
            // so a prefix spanning lines takes only a continuation token.
            return Ok(None);
        }
        if min == 0
            && (self.command_depth == 0 || self.groups > self.command_group)
            && (resumed.is_some() || self.tokens[self.pos].line == self.previous()?.end_line)
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
            if matches!(self.token(), Token::Words(_)) {
                self.percent_argument = self.pos;
            }
            return Ok(Some(Suffix::Command));
        }
        // A brace on the line of a call starts its block; anywhere else it
        // starts a hash literal, which cannot follow an expression.
        if self.token() == &Token::P('{')
            && self.tokens[self.pos].line == self.previous()?.end_line
            && self.block_follows(lhs)?
        {
            return Ok(Some(Suffix::Block(true)));
        }
        if matches!(self.token(), Token::Word(w) if w == "do" && !canonical())
            && (self.can_attach_do()
                || (self.pos != self.call_end && self.significant(self.call_end) == self.pos))
        {
            return Ok(Some(Suffix::Block(false)));
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
        if self.token() == &Token::P('?')
            && let Some(error) = self.diagnostic(self.pos)
        {
            return Err(error);
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
        // Go locates a binary expression at its operator's token; `//`,
        // which Go lacks, is located at its start.
        let offset = if op == "//" {
            self.tokens[self.pos].offset
        } else {
            self.position(self.pos)
        } as u32;
        Ok((left >= min).then_some(Suffix::Binary(op, right, offset)))
    }
    fn member_name(&mut self) -> Result<Name> {
        match self.token() {
            Token::Word(name) if !name.starts_with('@') => {
                let name = *name;
                self.method_spelling(&name, self.tokens[self.pos].offset)?;
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
    /// Reports whether parentheses call the value of a begin expression. The
    /// selected computed-call policy keeps this call where Go's line limit
    /// would end the expression; see docs/computed-calls.md.
    fn begin_call(&self, lhs: &Expr) -> bool {
        self.token() == &Token::P('(')
            && matches!(&lhs.node, Node::Try(attempt) if !attempt.modifier)
    }
    /// Reports whether the token at `index` continues a line expression past
    /// its line, as Go's `lineLimitedContinuationToken` does.
    fn limit_continues(&self, index: usize) -> Result<bool> {
        Ok(match &self.tokens[index].token {
            Token::P('.' | '?') => true,
            Token::Words(words) => words.ambiguous,
            Token::Op("*") => !self.splat_assignment_ahead(index)?,
            Token::Op("+" | "-") => {
                let sign = &self.tokens[index];
                let next = &self.tokens[self.significant(index + 1)];
                next.token != Token::Eof
                    && (next.line > sign.line || (next.line == sign.line && next.offset > sign.end))
            }
            Token::Op(op) => matches!(
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
        })
    }
    /// Refuses a hash literal, at offset `brace`, among the arguments of a
    /// parenless call whose first argument starts at token `start`. The fix
    /// gives the call parentheses; without them a hash first after the call
    /// would read as a block.
    fn hash_argument(&self, start: usize, brace: usize) -> Error {
        let callee = &self.tokens[start - 1];
        let name = match &callee.token {
            Token::Word(name) => name.as_str(),
            _ => "the call",
        };
        let message = format!(
            "a hash literal passed to `{name}` needs parentheses, as in `{name}(..., {{ ... }})`; after a call, `{{` starts a block"
        );
        let span = crate::diagnostic::Span::new(brace, brace + 1);
        let edits = vec![
            crate::diagnostic::Edit {
                span: crate::diagnostic::Span::new(callee.end, self.tokens[start].offset),
                replacement: "(".to_owned(),
            },
            crate::diagnostic::Edit {
                span: crate::diagnostic::Span::at(self.previous().map_or(brace, |last| last.end)),
                replacement: ")".to_owned(),
            },
        ];
        let diagnostic = crate::diagnostic::Diagnostic::error(
            crate::diagnostic::Code::HASH_ARGUMENT,
            span,
            &message,
        )
        .with_fix(crate::diagnostic::Fix::edits(
            "give the call parentheses",
            edits,
        ));
        Error::syntax(self.work, brace, message).with_diagnostic(diagnostic)
    }

    /// Whether the `{` at the current token starts `lhs`'s block: it follows
    /// a `)`, or `lhs` is a call.
    fn block_follows(&self, lhs: &Expr) -> Result<bool> {
        Ok(self.tokens[self.pos - 1].token == Token::P(')') || self.block_owner(lhs)?)
    }
    /// Whether `lhs` is a call that a `{` after it gives a block: a function
    /// or method name or a call with arguments, and not a value such as a
    /// local, a constant or a literal. After a value, the brace is left for
    /// the parenless call whose last argument the value is.
    fn block_owner(&self, lhs: &Expr) -> Result<bool> {
        Ok(match &lhs.node {
            Node::Var(name) => {
                !(name == "self"
                    || name.starts_with('@')
                    || name.chars().next().is_some_and(unicode::upper)
                    || self.locals.contains(self.work, name)?)
            }
            Node::Scope(_, name, args) => {
                args.is_some() || !name.chars().next().is_some_and(unicode::upper)
            }
            Node::Call(..)
            | Node::Method(..)
            | Node::SafeMethod(..)
            | Node::Member(..)
            | Node::SafeMember(..)
            | Node::ComputedCall(..) => true,
            _ => false,
        })
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
            // Go continues a line-limited expression onto a `do` only right
            // after a parenthesized call with arguments.
            Token::Word(ref word) if word == "do" && !canonical() => {
                (self.line_exprs == 0 && self.can_attach_do()) || self.pos == self.call_end
            }
            Token::P('.') | Token::Op("::" | "&.") => true,
            Token::P('?') => min <= 2,
            // Outside a line expression, Go reads any suffix after a line
            // break, but a block starts on its call's line.
            Token::P('(' | '[') => self.line_exprs == 0 && self.groups > 0,
            Token::Word(ref word) if word == "rescue" => {
                self.line_exprs == 0 && self.groups > 0 && min == 0 && !self.keyword_label(next)
            }
            Token::Op(op) => {
                let Some((left, _)) = binding_power(op) else {
                    return Ok(None);
                };
                if left < min {
                    return Ok(None);
                }
                if self.line_exprs == 0 {
                    if matches!(op, "/" | "//") {
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
                    "/" | "//" => false,
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
    /// Refuses an argument after keywords unless Go lets it follow them: a
    /// keyword, a keyword splat or a block argument.
    fn keyword_order(&self, message: &str) -> Result<()> {
        if matches!(self.token(), Token::Op("**" | "&")) || self.keyword_label(self.pos) {
            return Ok(());
        }
        self.err(message)
    }
    fn keyword_label(&self, pos: usize) -> bool {
        matches!(&self.tokens[pos].token, Token::Word(word) if !word.starts_with('@'))
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
                Node::Var(_) | Node::Member(..) | Node::SafeMember(..) | Node::Scope(_, _, None)
            )
        {
            return Ok(false);
        }
        let local = match &lhs.node {
            // Like Go, only an identifier or member can take arguments.
            Node::Var(name) if name == "self" || name.starts_with('@') => return Ok(false),
            Node::Var(name) => self.locals.contains(self.work, name)?,
            // A scoped function takes arguments, as in `Math::sqrt 9`; a
            // constant, nested type or enum member takes none.
            Node::Scope(_, name, _) if name.chars().next().is_some_and(unicode::upper) => {
                return Ok(false);
            }
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
            Token::P('[') => !local && previous.end != next.offset,
            // Only a declared local makes `%w` a modulo. An implicit block `it`
            // still calls a function named `it`, as in Go.
            Token::Words(..) => {
                let implicit =
                    matches!(&lhs.node, Node::Var(name) if name == "it") && !self.declared_it;
                (!local || implicit) && previous.end != next.offset
            }
            Token::Regex(..) => !local && previous.end != next.offset,
            // A command's argument may start with a regex, such as `puts /x/`,
            // which the lexer read as division. Floor division needs an
            // operand, so `puts //` at the end of a line passes an empty regex.
            Token::Op(op @ ("/" | "//")) => {
                !local
                    && previous.end != next.offset
                    && (self
                        .source
                        .as_bytes()
                        .get(next.end)
                        .is_some_and(|byte| !matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
                        || (op == "//"
                            && matches!(
                                self.tokens[self.pos + 1].token,
                                Token::EndLine | Token::Eof
                            )))
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
            | Token::Words(..)
            | Token::Symbol(_)
            | Token::QuotedSymbol(_) => true,
            Token::Invalid(invalid) => matches!(invalid.failure, Failure::Literal(_)),
            Token::Op("!") => true,
            Token::P('[') | Token::Op("*" | "**" | "&") => after_comma,
            _ => false,
        }
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
        types: bool,
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
            return self.argument_type_literal(types);
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
            | Token::Words(..)
            | Token::Symbol(_)
            | Token::QuotedSymbol(_) => true,
            Token::Invalid(invalid) => matches!(invalid.failure, Failure::Literal(_)),
            Token::P('(' | '[' | '{') | Token::Op("+" | "-" | "!") => true,
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
        "=" | "+=" | "-=" | "*=" | "/=" | "//=" | "%=" | "**=" | "||=" | "&&="
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
        "*" | "/" | "//" | "%" => (12, 13),
        "**" => (14, 14),
        _ => return None,
    })
}

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
/// Go's bound on source text quoted in a diagnostic: at most 64 bytes, cut at
/// a character boundary and marked.
pub(super) struct SourceText<'a>(&'a str);

pub(super) fn source_text(text: &str) -> SourceText<'_> {
    SourceText(text)
}

impl std::fmt::Display for SourceText<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.len() <= 64 {
            return f.write_str(self.0);
        }
        let mut end = 64;
        while !self.0.is_char_boundary(end) {
            end -= 1;
        }
        write!(f, "{}...", &self.0[..end])
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
