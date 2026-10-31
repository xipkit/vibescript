use crate::{
    Error, Result, Value,
    compilation::{Boxed, Buffer, Bytes, Frame, Name, Table, Tasks, Text, Work, framed, task},
};
use std::cell::{RefCell, RefMut};

mod ast;
mod blocks;
mod calls;
mod classes;
mod continuation;
mod control;
mod cursor;
mod declarations;
mod errors;
mod expressions;
mod lexer;
mod literals;
pub(crate) mod modules;
mod names;
pub(crate) mod record;
mod recovery;
mod statements;
mod suffixes;
mod targets;
mod teardown;
mod tokens;
pub(crate) mod typed;
mod types;
pub(crate) mod unicode;
mod work;
pub(crate) use ast::{
    Argument, ArgumentKind, Block, BlockParam, CallForm, Declarations, Definition, Expr,
    Interpolated, Node, Outline, ParamKind, Parameter, Rescue, Statement, Stmt, Target, Try,
    TypeAlias, When,
};
use calls::call_depth;
use continuation::Suffix;
use cursor::{Label, source_text};
use lexer::{Failure, Lexeme, Part, Token, lex};
use literals::Leaf;
pub(crate) use names::{
    HostName, KEYWORDS, binding_name, host_function_name, host_method_name, identifier, keyword,
};
use names::{def_operator, name_suffix_position, reserved};
use statements::assignment;
use targets::{Place, duplicate_parameter};
use tokens::Tokens;

// Go's maxSyntaxDepth bounds both parser recursion and syntax tree height.
const MAX_DEPTH: usize = 1024;
pub(crate) const MAX_SOURCE: usize = 8 << 20;
pub(crate) const TOO_DEEP: &str = "syntax nesting too deep";
const ADJACENT_EXPRESSIONS: &str = "adjacent expressions need a separator; insert an operator, a comma between arguments, or a newline (or `;`) between statements";

fn parser<'a>(source: &'a str, work: &'a dyn crate::compilation::Work) -> Result<Parser<'a>> {
    parser_from_tokens(source, work, Tokens::new(lex(source, work)?, work)?)
}

fn parser_from_tokens<'a>(
    source: &'a str,
    work: &'a dyn Work,
    tokens: Tokens<'a>,
) -> Result<Parser<'a>> {
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
        suffixed: RefCell::default(),
        target_start: None,
        namespace: None,
        namespace_body: false,
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
    let error = canonical_error_mode(source, work, recover).unwrap_or_else(|| {
        if recover {
            recovery::diagnostics(source, error, work)
        } else {
            error
        }
    });
    suffixes::extend_fixes(source, work, error)
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
    /// The names bound in scope, each with its suffixed binding's id in a
    /// [`lenient`] parse, or 0.
    locals: Table<u32>,
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
    /// The uses of suffixed bindings a lenient parse records.
    suffixed: RefCell<suffixes::Uses>,
    /// The token that starts the destructuring target being read, whose
    /// name, if that is all the target is, binds.
    target_start: Option<usize>,
    /// In a lenient parse, the class or module whose body or method is read,
    /// from which scoped names resolve, by its id in the recorded uses.
    namespace: Option<u32>,
    /// Whether the statement is directly in that body, where a constant
    /// belongs to the namespace.
    namespace_body: bool,
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

/// Runs the recursive grammar over shared parser state, so source nesting
/// grows a heap task stack instead of the native one.
struct Parsing<'a, M: recovery::Mode = recovery::FailFast> {
    parser: RefCell<Parser<'a>>,
    tasks: Tasks<Call, Parsed>,
    recovery: RefCell<M::State>,
    /// The parser's work, which the boxed steps of its tasks charge.
    work: &'a dyn Work,
}

impl<'a, M: recovery::Mode> Parsing<'a, M> {
    fn new(parser: Parser<'a>) -> Self {
        let work = parser.work;
        Self {
            parser: RefCell::new(parser),
            tasks: Tasks::new(),
            recovery: RefCell::new(M::State::default()),
            work,
        }
    }

    fn p(&self) -> RefMut<'_, Parser<'a>> {
        self.parser.borrow_mut()
    }

    fn run(&self, call: Call) -> Result<Parsed> {
        self.tasks.run(call, |call| self.start(call), self.work)
    }

    fn start(&self, call: Call) -> Result<Frame<'_, Parsed>> {
        let work = self.work;
        match call {
            Call::Program => task(work, async { Ok(Parsed::Program(self.program().await?)) }),
            Call::Interpolation => {
                task(work, async { Ok(Parsed::Expr(self.interpolated().await?)) })
            }
            Call::Expr(min) => task(work, async move {
                Ok(Parsed::Expr(self.expression(min).await?))
            }),
            Call::Tail(lhs, suffix, min) => task(work, async move {
                let result = self.expr_tail(lhs, min, Some(suffix), None, false).await;
                self.p().depth -= 1;
                Ok(Parsed::Expr(result?))
            }),
            Call::Block(stop) => task(work, async move {
                Ok(Parsed::Body(self.block_task(stop).await?))
            }),
            Call::Target(typed) => task(work, async move {
                let (target, tuple) = self.target(Place::Group(typed)).await?;
                Ok(Parsed::Target(target, tuple))
            }),
            Call::Class => task(work, async {
                Ok(Parsed::Module(self.class_like(false).await?))
            }),
            Call::Module => task(work, async {
                Ok(Parsed::Module(self.class_like(true).await?))
            }),
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
}

pub(crate) fn unsupported(work: &dyn Work, message: &str) -> Error {
    crate::compilation::error(work, None, format_args!("{message}"))
}
