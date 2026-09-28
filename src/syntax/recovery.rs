use super::*;
use crate::{
    ErrorKind,
    diagnostic::{Code, Diagnostic, Span},
};
use std::cell::Cell;

const MAX_ERRORS: usize = 100;

thread_local! {
    static LEXICAL: Cell<bool> = const { Cell::new(false) };
}

pub(super) fn lexical() -> bool {
    LEXICAL.with(Cell::get)
}

#[derive(Default)]
pub(super) struct Recovery {
    errors: Vec<Error>,
}

/// Replays only a failed host parse. The ordinary parser has no recovery
/// checkpoints or scope copies, and invocation compilation remains fail-fast.
pub(super) fn diagnostics(source: &str, first: Error, caller: &dyn Work) -> Error {
    diagnostics_with_tokens(source, first, caller, None)
}

pub(super) fn diagnostics_with_tokens(
    source: &str,
    first: Error,
    caller: &dyn Work,
    tokens: Option<Tokens<'_>>,
) -> Error {
    if first.kind != ErrorKind::Syntax || first.message == TOO_DEEP || source.len() > MAX_SOURCE {
        return first;
    }
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            LEXICAL.with(|flag| flag.set(self.0));
        }
    }
    let _restore = Restore(LEXICAL.with(|flag| flag.replace(true)));
    let work = RecoveryWork {
        remaining: Cell::new(source.len().saturating_mul(128).saturating_add(16384)),
        caller,
    };
    let parser = if let Some(tokens) = tokens {
        parser_from_tokens(source, &work, tokens)
    } else {
        let Ok(parser) = parser(source, &work) else {
            return caller.checkpoint().err().unwrap_or(first);
        };
        parser
    };
    let parsing = Parsing::<Recover>::new(parser);
    if let Err(error) = parsing.run(Call::Program) {
        parsing.recovery.borrow_mut().record(error);
    }
    if let Err(error) = caller.checkpoint() {
        return error;
    }
    let mut diagnostics = vec![diagnostic(source, &first)];
    for error in parsing.recovery.into_inner().errors {
        let next = diagnostic(source, &error);
        if !diagnostics
            .iter()
            .any(|d| d.span == next.span && d.code == next.code && d.message == next.message)
        {
            diagnostics.push(next);
        }
        if diagnostics.len() == MAX_ERRORS {
            break;
        }
    }
    first.with_diagnostics(diagnostics)
}

// A failed parse may revisit nested regions and copy enclosing scopes. Bound
// that work as well as the diagnostic count, even without a caller budget.
// Exhaustion keeps the diagnostics already found, never adding a quota error.
struct RecoveryWork<'a> {
    remaining: Cell<usize>,
    caller: &'a dyn Work,
}

impl Work for RecoveryWork<'_> {
    fn unmetered(&self) -> bool {
        self.caller.unmetered()
    }
    fn charge(&self, steps: usize) -> Result<()> {
        let remaining = self
            .remaining
            .get()
            .checked_sub(steps)
            .ok_or_else(|| Error::new(ErrorKind::Steps, "recovery work exhausted"))?;
        self.remaining.set(remaining);
        self.caller.charge(steps)?;
        Ok(())
    }
    fn bytes(&self, bytes: usize) -> Result<()> {
        self.caller.bytes(bytes)?;
        self.charge(bytes / 4096 + 1)
    }
    fn checkpoint(&self) -> Result<()> {
        self.caller.checkpoint()?;
        self.charge(1)
    }
    fn reserve(&self, bytes: usize) -> Result<Option<crate::budget::Charge>> {
        self.checkpoint()?;
        self.caller.reserve(bytes)
    }
    fn allocation_error(&self, message: &str) -> Error {
        self.caller.allocation_error(message)
    }
}

fn diagnostic(source: &str, error: &Error) -> Diagnostic {
    if let Some(diagnostic) = error.diagnostics().first() {
        return diagnostic.clone();
    }
    let start = error.offset.unwrap_or(0).min(source.len());
    let end = start + source[start..].chars().next().map_or(0, char::len_utf8);
    Diagnostic::error(Code::SYNTAX, Span::new(start, end), error.message.clone())
}

impl Recovery {
    fn record(&mut self, error: Error) {
        if error.kind == ErrorKind::Syntax
            && self.errors.len() < MAX_ERRORS
            && !self
                .errors
                .iter()
                .any(|e| e.offset == error.offset && e.message == error.message)
        {
            self.errors.push(error);
        }
    }
}

pub(super) struct Checkpoint {
    start: usize,
    errors: usize,
    depth: usize,
    groups: usize,
    line_exprs: usize,
    command_depth: usize,
    ternaries: usize,
    command_group: usize,
    loop_condition: Option<usize>,
    then_stop: Option<usize>,
    type_call: bool,
    type_argument: bool,
    type_structural_error: bool,
    nesting: usize,
    call_end: usize,
    percent_argument: usize,
}

pub(super) struct Scope<'p, 'a> {
    parser: &'p RefCell<Parser<'a>>,
    locals: Option<Table<()>>,
    declared_it: bool,
    block_name: Option<Name>,
    inside_class: bool,
    nesting: usize,
}

impl Drop for Scope<'_, '_> {
    fn drop(&mut self) {
        let mut p = self.parser.borrow_mut();
        p.locals = self.locals.take().unwrap();
        p.declared_it = self.declared_it;
        p.block_name = self.block_name.take();
        p.inside_class = self.inside_class;
        p.nesting = self.nesting;
    }
}

// Zero-sized state, checkpoints and scope guards keep successful parses from
// growing their async frames or allocating recovery bookkeeping.
pub(super) trait Mode {
    const RECOVER: bool;
    type State: Default;
    type Checkpoint;
    type Scope<'p, 'a>
    where
        'a: 'p;
    fn checkpoint(parser: &RefCell<Parser<'_>>, state: &RefCell<Self::State>) -> Self::Checkpoint;
    fn scope<'p, 'a: 'p>(parser: &'p RefCell<Parser<'a>>) -> Result<Self::Scope<'p, 'a>>;
    fn recover(
        parser: &RefCell<Parser<'_>>,
        state: &RefCell<Self::State>,
        checkpoint: Self::Checkpoint,
        stop: &[&str],
        error: Error,
    ) -> Result<()>;
    fn first(state: &RefCell<Self::State>) -> Option<Error>;
}

pub(super) struct FailFast;
impl Mode for FailFast {
    const RECOVER: bool = false;
    type State = ();
    type Checkpoint = ();
    type Scope<'p, 'a>
        = ()
    where
        'a: 'p;
    fn checkpoint(_: &RefCell<Parser<'_>>, _: &RefCell<()>) {}
    fn scope<'p, 'a: 'p>(_: &'p RefCell<Parser<'a>>) -> Result<()> {
        Ok(())
    }
    fn recover(
        _: &RefCell<Parser<'_>>,
        _: &RefCell<()>,
        _: (),
        _: &[&str],
        error: Error,
    ) -> Result<()> {
        Err(error)
    }
    fn first(_: &RefCell<()>) -> Option<Error> {
        None
    }
}

pub(super) struct Recover;
impl Mode for Recover {
    const RECOVER: bool = true;
    type State = Recovery;
    type Checkpoint = Checkpoint;
    type Scope<'p, 'a>
        = Scope<'p, 'a>
    where
        'a: 'p;
    fn first(state: &RefCell<Recovery>) -> Option<Error> {
        state.borrow().errors.first().cloned()
    }
    fn scope<'p, 'a: 'p>(parser: &'p RefCell<Parser<'a>>) -> Result<Scope<'p, 'a>> {
        let p = parser.borrow();
        Ok(Scope {
            parser,
            locals: Some(p.locals.copy(p.work)?),
            declared_it: p.declared_it,
            block_name: p.block_name.clone(),
            inside_class: p.inside_class,
            nesting: p.nesting,
        })
    }
    fn checkpoint(parser: &RefCell<Parser<'_>>, state: &RefCell<Recovery>) -> Checkpoint {
        let p = parser.borrow();
        Checkpoint {
            start: p.pos,
            errors: state.borrow().errors.len(),
            depth: p.depth,
            groups: p.groups,
            line_exprs: p.line_exprs,
            command_depth: p.command_depth,
            ternaries: p.ternaries.len(),
            command_group: p.command_group,
            loop_condition: p.loop_condition,
            then_stop: p.then_stop,
            type_call: p.type_call,
            type_argument: p.type_argument,
            type_structural_error: p.type_structural_error,
            nesting: p.nesting,
            call_end: p.call_end,
            percent_argument: p.percent_argument,
        }
    }
    fn recover(
        parser: &RefCell<Parser<'_>>,
        state: &RefCell<Recovery>,
        c: Checkpoint,
        stop: &[&str],
        error: Error,
    ) -> Result<()> {
        if error.kind != ErrorKind::Syntax
            || error.message == TOO_DEEP
            || state.borrow().errors.len() >= MAX_ERRORS
        {
            return Err(error);
        }
        // A failed enclosing construct does not add an EOF/closer cascade
        // after an error already recovered inside its body.
        if state.borrow().errors.len() == c.errors {
            state.borrow_mut().record(error);
        }
        let mut p = parser.borrow_mut();
        p.synchronize(c.start, stop)?;
        p.depth = c.depth;
        p.groups = c.groups;
        p.line_exprs = c.line_exprs;
        p.command_depth = c.command_depth;
        p.ternaries.truncate(c.ternaries);
        p.command_group = c.command_group;
        p.loop_condition = c.loop_condition;
        p.then_stop = c.then_stop;
        p.type_call = c.type_call;
        p.type_argument = c.type_argument;
        p.type_structural_error = c.type_structural_error;
        p.nesting = c.nesting;
        p.call_end = c.call_end;
        p.percent_argument = c.percent_argument;
        Ok(())
    }
}

impl<'a, M: Mode> Parsing<'a, M> {
    pub(super) fn recovery_scope(&self) -> Result<M::Scope<'_, 'a>> {
        M::scope(&self.parser)
    }
    pub(super) fn recovery_checkpoint(&self) -> M::Checkpoint {
        M::checkpoint(&self.parser, &self.recovery)
    }
    pub(super) fn recovered_error(&self) -> Option<Error> {
        M::first(&self.recovery)
    }
    pub(super) fn recover(
        &self,
        checkpoint: M::Checkpoint,
        stop: &[&str],
        error: Error,
    ) -> Result<()> {
        M::recover(&self.parser, &self.recovery, checkpoint, stop, error)
    }
}

impl Parser<'_> {
    fn synchronize(&mut self, start: usize, stop: &[&str]) -> Result<()> {
        let mut delimiters = Vec::new();
        let mut blocks = 0usize;
        let mut statement = true;
        let mut index = start;
        let failed = self.pos.max(start + 1).min(self.tokens.len() - 1);
        while index < self.tokens.len() - 1 {
            self.work.charge(1)?;
            let token = &self.tokens[index];
            let word = match &token.token {
                Token::Word(w) => w.as_str(),
                _ => "",
            };
            if index >= failed {
                if blocks == 0
                    && (stop.contains(&word)
                        || token.token == Token::P('}')
                            && stop.contains(&"}")
                            && !delimiters.contains(&'{'))
                {
                    break;
                }
                if matches!(word, "def" | "class" | "module" | "enum") {
                    break;
                }
                if blocks == 0
                    && index > start
                    && self.tokens[index - 1].token == Token::EndLine
                    && matches!(token.token, Token::Word(_))
                    && self
                        .tokens
                        .get(index + 1)
                        .is_some_and(|next| next.token == Token::Op("="))
                {
                    break;
                }
                if token.token == Token::EndLine && blocks == 0 && delimiters.is_empty() {
                    index += 1;
                    break;
                }
                if blocks == 0
                    && delimiters.is_empty()
                    && index > start
                    && self.tokens[index - 1].token == Token::EndLine
                {
                    break;
                }
            }
            match &token.token {
                Token::P(open @ ('(' | '[' | '{')) => delimiters.push(*open),
                Token::P(close @ (')' | ']' | '}')) => {
                    let open = match close {
                        ')' => '(',
                        ']' => '[',
                        _ => '{',
                    };
                    if let Some(matched) = delimiters.iter().rposition(|&c| c == open) {
                        delimiters.truncate(matched);
                    }
                }
                Token::Word(w)
                    if matches!(
                        w.as_str(),
                        "def" | "class" | "module" | "enum" | "begin" | "case"
                    ) || statement
                        && matches!(w.as_str(), "if" | "unless" | "while" | "until" | "for") =>
                {
                    blocks += 1
                }
                Token::Word(w) if w == "end" => blocks = blocks.saturating_sub(1),
                _ => (),
            }
            statement = token.token == Token::EndLine;
            index += 1;
        }
        self.pos = index.max(start + 1).min(self.tokens.len() - 1);
        Ok(())
    }
}
