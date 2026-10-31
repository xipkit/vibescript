use crate::{
    Error, Result, Value,
    compilation::{Boxed, Buffer, Bytes, Frame, Name, Table, Tasks, Text, Work, framed, task},
};
use std::cell::{RefCell, RefMut};

mod ast;
mod classes;
mod continuation;
mod cursor;
mod declarations;
mod errors;
mod lexer;
mod literals;
pub(crate) mod modules;
mod names;
pub(crate) mod record;
mod recovery;
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
use continuation::Suffix;
use cursor::{Label, source_text};
use lexer::{Failure, Lexeme, Part, Token, lex};
use literals::Leaf;
pub(crate) use names::{
    HostName, KEYWORDS, binding_name, host_function_name, host_method_name, identifier, keyword,
};
use names::{def_operator, name_suffix_position, reserved};
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
        framed(self.work, self.block_expression(expr, brace))?.await
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
        let stmt = self
            .modified_statement(offset)
            .await?
            .at(self.work, offset)?;
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
            framed(self.work, self.begin_statement())?.await?
        } else {
            self.plain_statement().await?
        };
        if matches!(
            stmt,
            Statement::If(..) | Statement::While(..) | Statement::For(..)
        ) {
            stmt = framed(self.work, self.continued_statement(stmt, offset))?.await?;
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
            framed(self.work, self.reject_modifier())?.await?;
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
        let body = Buffer::from_array(work, [stmt.at(work, offset)?])?;
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
            let stmt = stmt.at(p.work, offset)?;
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
            if !target.is_binding(p.work)? {
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
            Token::Word(w) => framed(self.work, self.word_expression(w.as_str(), offset))?.await,
            Token::P('(') => self.group_expression().await,
            Token::P('[') => self.array_expression().await,
            Token::P('{') => framed(self.work, self.hash_expr())?.await,
            Token::Op(op @ (".." | "...")) => {
                framed(self.work, self.open_range_expression(op))?.await
            }
            Token::Op(op @ ("-" | "+" | "!")) => framed(self.work, self.unary_prefix(op))?.await,
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
        let stmt = stmt.at(self.work, offset)?;
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
                Suffix::Rescue => framed(self.work, self.rescue_modifier(lhs))?.await,
                Suffix::Command => self.command_expression(lhs).await,
                Suffix::Block(brace) => framed(self.work, self.block_expression(lhs, brace))?.await,
                Suffix::Call => {
                    let args = self.call_arguments().await?;
                    self.p().parenthesized_call(lhs, args)
                }
                Suffix::Scope => framed(self.work, self.scoped_expression(lhs))?.await,
                Suffix::Member(safe) => self.member_expression(lhs, safe).await,
                Suffix::Index(offset) => self.index_expression(lhs, offset).await,
                Suffix::Ternary(offset) => {
                    framed(self.work, self.ternary_expression(lhs, offset))?.await
                }
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
            framed(self.work, self.hash_block(&lhs))?.await?;
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
        let mut params = 0;
        for target in block.params.iter() {
            params = params.max(target.depth(self.work)?);
        }
        let body = block.body.iter().map(|s| s.depth).max().unwrap_or(0);
        let depth = 1 + lhs.depth.max(1 + body.max(params));
        let p = self.p();
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
            let at = p.tokens[p.pos].offset;
            let Token::Word(name) = p.bump()? else {
                unreachable!()
            };
            // A name after `::` is spelled as one after `.` is; only one with
            // a `?` or `!` can be spelled wrong.
            if name.contains(['?', '!']) {
                p.read_spelling(&name, at)?;
            }
            let continues = matches!(
                p.tokens[p.significant(p.pos)].token,
                Token::Op("::") | Token::P('.')
            );
            let called = p.token() == &Token::P('(');
            p.scoped_read(&lhs, &name, at, continues, called)?;
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
            // `M.X?` reads a namespace's member as `M::X?` does.
            let at = p.tokens[p.pos - 1].offset;
            let called = p.token() == &Token::P('(');
            p.scoped_read(&lhs, &name, at, false, called)?;
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
                    p.declare_block_parameter(&target, &params)?;
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
            p.locals.insert(work, Name::new(work, "it")?, 0)?;
            for n in ["_1", "_2", "_3", "_4", "_5", "_6", "_7", "_8", "_9"] {
                p.locals.insert(work, Name::new(work, n)?, 0)?;
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
        if !target.is_binding(work)? {
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
    fn member_name(&mut self) -> Result<Name> {
        match self.token() {
            Token::Word(name) if !name.starts_with('@') => {
                let name = *name;
                let offset = self.tokens[self.pos].offset;
                self.bump()?;
                self.read_spelling(&name, offset)?;
                Name::new(self.work, &name)
            }
            Token::Op("<=>") => {
                self.bump()?;
                Name::new(self.work, "<=>")
            }
            _ => self.expected(Label::Text("member name")),
        }
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

pub(crate) fn unsupported(work: &dyn Work, message: &str) -> Error {
    crate::compilation::error(work, None, format_args!("{message}"))
}
