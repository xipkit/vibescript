//! The static type checker of ADR-007.
//!
//! The checker reads a parsed program and reports every type error as a
//! [`Diagnostic`], through [`crate::Engine::compile`] and
//! [`crate::Engine::type_check`]. It checks each function once, from its own
//! signature and the signatures of what it calls, never from a callee's body, so its work
//! is linear in the program. Besides diagnostics it records the static type of
//! every member call's receiver in [`CallTypes`], which rules that depend on
//! the receiver's type, such as typed renames of removed spellings, consult.
//!
//! The pass has four parts. `ty` interns types, so comparing two is comparing
//! ids, and decides assignability. `program` collects the declarations and
//! resolves every signature, alias and instance variable before any body is
//! checked; `sigs` reads the builtin signature table and host signatures into
//! the same form, with type variables and bounds. `check`, `expr` and `calls`
//! walk each body once: `flow` keeps each local's narrowed type and
//! definite assignment on one state with a trail of changes, so branches and
//! loops join in time proportional to what they changed, and `assigns` lists
//! what each `begin`, loop and block assigns in one walk. `modules` resolves
//! and checks the files a program requires, and `spans` turns the syntax
//! tree's start offsets into exact spans from the parser's tokens.

use crate::{capability::Registered, diagnostic::Diagnostic, syntax::Declarations};
use std::{
    collections::{HashMap, HashSet},
    fmt,
};

mod assigns;
mod calls;
mod check;
mod construction;
mod expr;
mod flow;
mod foreign;
mod modules;
mod program;
mod sigs;
mod spans;
mod ty;

/// What the checker reads: one source, parsed, with the host functions and
/// capabilities its engine registers.
pub(crate) struct Input<'a> {
    pub source: &'a str,
    pub parsed: &'a Declarations,
    /// The tokens the parser read, for exact spans.
    pub tokens: &'a [crate::tooling::Token],
    pub hosts: Vec<(&'a String, &'a Registered)>,
    /// The globals and capabilities the host declares for every call.
    pub declared: &'a crate::declared::Declarations,
    /// Whether the source is a required file rather than a host script.
    pub file: bool,
    pub origin: Option<&'a crate::loading::Origin>,
    /// Finds the source and filename of a module `require` names, when the
    /// engine can load modules.
    pub modules: Option<&'a Modules<'a>>,
    /// What the check may spend before it stops; unlimited unless metered.
    pub budget: crate::compilation::Budget,
}

/// Resolves a required module's name to its source and filename.
pub(crate) type Modules<'a> = dyn Fn(&str, Option<&crate::loading::Origin>) -> crate::Result<(String, crate::loading::Origin)>
    + Sync
    + 'a;

/// Sources longer than this are checked on a thread with [`STACK`] bytes of
/// stack: the checker recurses once per level of syntax, which the parser
/// bounds, and a short source cannot nest deeply.
#[cfg(not(target_os = "wasi"))]
const SHALLOW: usize = 1024;
#[cfg(not(target_os = "wasi"))]
const STACK: usize = 64 << 20;

/// The tallest syntax the checker descends into on WASI. WASI has no
/// threads, so the checker and the surface walk recurse on the host's stack,
/// whose default size holds only about 220 levels of a debug build's
/// costliest recursion; taller syntax is refused there instead of exhausting
/// it. Elsewhere the parser's bound of 1,024 applies.
const HEIGHT: u32 = 128;

/// Whether syntax of this height is too tall to check on this platform.
fn too_tall(height: u32) -> bool {
    cfg!(target_os = "wasi") && height > HEIGHT
}

/// The result of checking one source.
#[derive(Clone, Debug, Default)]
pub struct Checked {
    /// Every finding in source order; any error stops compilation.
    pub diagnostics: Vec<Diagnostic>,
    /// The receiver type at each member call.
    pub calls: CallTypes,
    /// A deterministic count of the checker's work, which grows linearly
    /// with the program; compilation charges it to the step quota.
    pub steps: u64,
    /// What a required file exports, with the types its declarations give
    /// them.
    pub(crate) exported: Option<std::sync::Arc<modules::Exported>>,
    /// The top-level locals a host script assigns on every path, in name
    /// order, each with its declared type as an annotation writes it, or
    /// `any` where no annotation can name the type, such as a class used as
    /// a value. A host continuing a session, as `vibes repl` does, declares
    /// them for the next script.
    pub locals: Vec<(String, String)>,
    /// The type of the value the top-level statements produce, which
    /// `Script::run` returns, written the same way; `None` for a required
    /// file.
    pub result: Option<String>,
    /// What the checker proved about expressions, which the compiler uses.
    pub(crate) facts: Facts,
    /// Whether the check stopped at its budget before it finished, so its
    /// findings are incomplete and compilation fails with the budget's error.
    pub(crate) stopped: bool,
}

/// What the checker proved about expressions and blocks, by syntax node, for
/// the compiler to read for the same nodes it compiles. A node checked more
/// than once, such as a call on each alternative of a union receiver, keeps
/// only what every check proved.
#[derive(Clone, Debug, Default)]
pub(crate) struct Facts {
    /// Whether the compiler keeps the runtime type checks the checker
    /// proves, which differential tests of the checker compare against
    /// ([`crate::Engine::set_keep_type_checks`]); not a checker fact.
    pub keep_type_checks: bool,
    /// The one static base type of each member call's receiver that has one
    /// the runtime binds builtins to; `None` where checks disagreed.
    bases: HashMap<usize, Option<crate::members::direct::Base>>,
    /// Whether each expression's value is plain: it can hold no host method
    /// or exported function, which only values of type `any`, capabilities
    /// and required modules can. The runtime skips its scan for them. Also
    /// records a single numeric type, when proved, for arithmetic dispatch.
    values: HashMap<usize, ValueFact>,
    /// Whether every parameter of each block is plain.
    blocks: HashMap<usize, bool>,
    /// For each member call whose receiver is always an instance of one
    /// class this source declares, and whose member is that class's method,
    /// the class's qualified name; `None` where checks disagreed.
    classes: HashMap<usize, Option<String>>,
    /// The instance methods whose result the checker proves, by the offset
    /// of their definition: no method of their class can read an instance
    /// variable before it is assigned ([`construction`]). The compiler moves
    /// definitions, so their offset names them.
    results: std::collections::HashSet<u32>,
}

/// A numeric type proved by the checker. Integers may use compact or big storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Number {
    Int,
    Float,
}

#[derive(Clone, Copy, Debug)]
struct ValueFact {
    plain: bool,
    number: Option<Number>,
}

impl Facts {
    fn record_base(
        &mut self,
        call: &crate::syntax::Expr,
        base: Option<crate::members::direct::Base>,
    ) {
        let recorded = self.bases.entry(key(call)).or_insert(base);
        if *recorded != base {
            *recorded = None;
        }
    }

    fn record_value(&mut self, expr: &crate::syntax::Expr, plain: bool, number: Option<Number>) {
        let recorded = self
            .values
            .entry(key(expr))
            .or_insert(ValueFact { plain, number });
        recorded.plain &= plain;
        if recorded.number != number {
            recorded.number = None;
        }
    }

    fn record_class(&mut self, call: &crate::syntax::Expr, class: Option<String>) {
        let recorded = self
            .classes
            .entry(key(call))
            .or_insert_with(|| class.clone());
        if *recorded != class {
            *recorded = None;
        }
    }

    fn record_result(&mut self, def: &crate::syntax::Definition) {
        self.results.insert(def.offset);
    }

    fn record_block(&mut self, block: &crate::syntax::Block, plain: bool) {
        *self.blocks.entry(key(block)).or_insert(plain) &= plain;
    }

    /// The base the receiver of `call` always has, if one was recorded.
    pub(crate) fn base(&self, call: &crate::syntax::Expr) -> Option<crate::members::direct::Base> {
        self.bases.get(&key(call)).copied().flatten()
    }

    /// Whether `expr`'s value is plain; not when the checker did not see it.
    pub(crate) fn plain(&self, expr: &crate::syntax::Expr) -> bool {
        self.values.get(&key(expr)).is_some_and(|fact| fact.plain)
    }

    /// The numeric type every check of `expr` proved, if any.
    pub(crate) fn number(&self, expr: &crate::syntax::Expr) -> Option<Number> {
        self.values.get(&key(expr)).and_then(|fact| fact.number)
    }

    /// The class whose method `call` always calls, if one was recorded.
    pub(crate) fn class(&self, call: &crate::syntax::Expr) -> Option<&str> {
        self.classes.get(&key(call))?.as_deref()
    }

    /// Whether the checker proves the result of instance method `def`.
    pub(crate) fn proven_result(&self, def: &crate::syntax::Definition) -> bool {
        self.results.contains(&def.offset)
    }

    /// Whether every parameter of `block` is plain.
    pub(crate) fn plain_block(&self, block: &crate::syntax::Block) -> bool {
        self.blocks.get(&key(block)).copied().unwrap_or(false)
    }
}

/// A syntax node's identity, which is stable from checking to compiling.
fn key<T>(node: &T) -> usize {
    std::ptr::from_ref(node) as usize
}

/// What checking the top-level statements of a host script found, for
/// [`Checked::locals`] and [`Checked::result`].
pub(crate) struct Session {
    locals: Vec<(String, ty::Ty)>,
    result: ty::Ty,
}

/// The static type of the receiver at each member call, keyed by the byte
/// offset of the member's name.
///
/// For `items.include?(x)` the key is the offset of `include?`. Calls whose
/// receiver the checker could not type, such as ones in code that failed to
/// resolve, are absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallTypes {
    /// Entries sorted by offset.
    entries: Vec<(usize, ReceiverType)>,
}

impl CallTypes {
    /// The receiver type of the member call whose name starts at `offset`.
    pub fn receiver_at(&self, offset: usize) -> Option<&ReceiverType> {
        self.entries
            .binary_search_by_key(&offset, |(key, _)| *key)
            .ok()
            .map(|index| &self.entries[index].1)
    }

    /// Whether every receiver alternative declares the called member as a user method.
    pub fn user_method_at(&self, offset: usize) -> bool {
        self.receiver_at(offset)
            .is_some_and(|receiver| receiver.user_method)
    }

    /// Every recorded call, by member-name offset in source order.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &ReceiverType)> {
        self.entries.iter().map(|(offset, ty)| (*offset, ty))
    }

    /// The number of recorded calls.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no call was recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub(crate) fn from_entries(mut entries: Vec<(usize, ReceiverType)>) -> Self {
        entries.sort_by_key(|(offset, _)| *offset);
        entries.dedup_by_key(|(offset, _)| *offset);
        Self { entries }
    }
}

/// A receiver's static type, by its canonical name and the base type of each
/// alternative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiverType {
    name: String,
    bases: Vec<String>,
    user_method: bool,
}

impl ReceiverType {
    pub(crate) fn new(name: String, bases: Vec<String>) -> Self {
        Self {
            name,
            bases,
            user_method: false,
        }
    }

    /// The type as an annotation writes it, such as `hash<string, int>`,
    /// `{ name: string }`, `int?` or `Invoice`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The base type of each alternative, sorted and without repeats:
    /// `array` for arrays and tuples, `hash` for hashes and shapes, the
    /// scalar's name (`int`, `float`, `string`, `bool`, `nil`, `symbol`,
    /// `time`, `duration`, `money`, `range`, `regex`, `match_data`, `error`),
    /// `any`, `type` for a type literal, or the name of a class or enum for
    /// its instances and members. A namespace used as a receiver, such as an
    /// enum, class or module name, is `namespace`, and a capability the host
    /// declares with members is `host`.
    pub fn bases(&self) -> &[String] {
        &self.bases
    }

    /// Whether every alternative other than `nil` has base `base`, and there
    /// is at least one.
    pub fn is(&self, base: &str) -> bool {
        let mut others = self.bases.iter().filter(|b| *b != "nil").peekable();
        others.peek().is_some() && others.all(|b| b == base)
    }
}

impl fmt::Display for ReceiverType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// Checks one parsed source.
pub(crate) fn check(input: &Input<'_>) -> Checked {
    #[cfg(not(target_os = "wasi"))]
    if input.source.len() > SHALLOW {
        return std::thread::scope(|scope| {
            let checking = std::thread::Builder::new()
                .stack_size(STACK)
                .spawn_scoped(scope, || check_nested(input, 0));
            match checking {
                Ok(handle) => handle
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
                Err(_) => check_nested(input, 0),
            }
        });
    }
    check_nested(input, 0)
}

/// Checks a source `depth` requires deep.
fn check_nested(input: &Input<'_>, depth: usize) -> Checked {
    let mut checker = Checker {
        source: input.source,
        parsed: input.parsed,
        spans: spans::Spans::new(input.source, input.tokens, &input.parsed.interpolations),
        types: ty::Types::new(),
        program: program::Program::default(),
        converter: sigs::Converter::default(),
        diagnostics: Vec::new(),
        calls: Vec::new(),
        constants: HashMap::new(),
        frame: check::Frame::new(None, false, None, String::new()),
        purposes: Vec::new(),
        mute: 0,
        steps: 0,
        modules: modules::Required::new(input, depth),
        memo: None,
        write_chain: HashSet::new(),
        fetch_receivers: HashMap::new(),
        session: None,
        too_deep: false,
        facts: Facts::default(),
        construction: construction::Construction::default(),
        self_receiver: false,
        symbols_stay: None,
        storing_self: None,
        assigns: assigns::Assigns::default(),
        budget: input.budget.clone(),
        stopped: false,
        polls: 0,
    };
    for (name, host) in &input.hosts {
        let function = crate::signatures::host::function(name, host);
        let sig = checker
            .converter
            .convert_owned(&mut checker.types, &function, None)
            .host();
        checker
            .program
            .hosts
            .insert((*name).clone(), std::rc::Rc::new(sig));
    }
    checker.declare_hosts(input.declared);
    checker.program.file = input.file;
    checker.declare_program(input.parsed);
    checker.check_retained_declarations(input.declared, input.parsed);
    checker.check_all();
    let steps = checker.total_steps();
    let stopped = checker.stopped;
    let exported = input.file.then(|| std::sync::Arc::new(checker.export()));
    let (mut locals, result) = match checker.session.take() {
        Some(session) => (
            session
                .locals
                .into_iter()
                .map(|(name, ty)| (name, checker.types.annotation(ty)))
                .collect(),
            Some(checker.types.annotation(session.result)),
        ),
        None => (Vec::new(), None),
    };
    locals.sort();
    let too_deep = checker.too_deep;
    let mut diagnostics = checker.diagnostics;
    diagnostics.sort_by_key(|d| (d.span.start, d.span.end));
    diagnostics.dedup_by(|a, b| a.code == b.code && a.span == b.span && a.message == b.message);
    let mut checked = Checked {
        diagnostics,
        calls: CallTypes::from_entries(checker.calls),
        steps,
        exported,
        locals,
        result,
        facts: checker.facts,
        stopped,
    };
    // Removed spellings of the canonical surface are compile errors too,
    // unless the source is too tall to walk or the check stopped early.
    if !too_deep && !stopped {
        crate::surface::add_to(&mut checked, input.source, input.tokens);
    }
    checked
}

/// Checks that the command line can call `function` with `count` arguments,
/// which it passes as strings: each positional parameter they bind must
/// accept `string`, and a rest parameter `array<string>`.
pub(crate) fn entry_arguments(input: &Input<'_>, function: &str, count: usize) -> Vec<Diagnostic> {
    let mut checker = Checker {
        source: input.source,
        parsed: input.parsed,
        spans: spans::Spans::new(input.source, input.tokens, &input.parsed.interpolations),
        types: ty::Types::new(),
        program: program::Program::default(),
        converter: sigs::Converter::default(),
        diagnostics: Vec::new(),
        calls: Vec::new(),
        constants: HashMap::new(),
        frame: check::Frame::new(None, false, None, String::new()),
        purposes: Vec::new(),
        mute: 0,
        steps: 0,
        modules: modules::Required::new(input, 0),
        memo: None,
        write_chain: HashSet::new(),
        fetch_receivers: HashMap::new(),
        session: None,
        too_deep: false,
        facts: Facts::default(),
        construction: construction::Construction::default(),
        self_receiver: false,
        symbols_stay: None,
        storing_self: None,
        assigns: assigns::Assigns::default(),
        budget: input.budget.clone(),
        stopped: false,
        polls: 0,
    };
    checker.declare_hosts(input.declared);
    checker.declare_program(input.parsed);
    checker.check_retained_declarations(input.declared, input.parsed);
    checker.diagnostics.clear();
    checker.entry_arguments(function, count);
    checker.diagnostics
}

/// The state of one check.
pub(crate) struct Checker<'a> {
    source: &'a str,
    parsed: &'a Declarations,
    spans: spans::Spans<'a>,
    types: ty::Types,
    program: program::Program<'a>,
    converter: sigs::Converter,
    diagnostics: Vec<Diagnostic>,
    calls: Vec<(usize, ReceiverType)>,
    /// Constants of class and module bodies, by namespace and name.
    constants: HashMap<(Option<program::NsId>, String), ty::Ty>,
    frame: check::Frame,
    /// Why the value being checked against a type is checked, innermost last.
    purposes: Vec<check::Purpose>,
    /// While positive, diagnostics are dropped: a second look at code that
    /// was already checked.
    mute: u32,
    /// Work done outside the types, spans and flow of the current function.
    steps: u64,
    /// The modules the program requires, and the names they publish.
    modules: modules::Required<'a>,
    /// Expression types recorded while checking a call on one alternative
    /// of a union receiver, which the other alternatives replay.
    memo: Option<Memo>,
    /// The reads a write goes through, by node: the receivers of an index
    /// or member assignment's target and of a mutating call, down to their
    /// root. An index among them reads its element as present, since the
    /// runtime raises when it is missing, and no fix rewrites one, since a
    /// write through a rewritten read would reach a copy.
    write_chain: HashSet<usize>,
    /// Receiver types of optional indexed reads, for diagnostic fixes only.
    /// None when repeated checks disagree about the receiver.
    fetch_receivers: HashMap<usize, Option<ty::Ty>>,
    /// The top-level statements' locals and result, once checked.
    session: Option<Session>,
    /// Whether some syntax was too tall to check ([`HEIGHT`]).
    too_deep: bool,
    /// What the checker proved for the compiler.
    facts: Facts,
    /// Instance variable reads, to prove each is assigned first.
    construction: construction::Construction,
    /// Whether the `self` being checked is a method call's receiver, which
    /// [`construction`] records as the call instead.
    self_receiver: bool,
    /// Why a symbol literal checked now stays a symbol at runtime, when it
    /// does: no typed boundary between it and where it is stored or used
    /// turns it into the enum member its expected type names.
    symbols_stay: Option<&'static str>,
    /// The instance variable that `self` itself is being stored into, as in
    /// `@next = self`, which the store assigns rather than lets escape.
    storing_self: Option<String>,
    /// The names each `begin`, loop and block body assigns.
    assigns: assigns::Assigns<'a>,
    /// What the check may spend, and whether it stopped there.
    budget: crate::compilation::Budget,
    stopped: bool,
    /// Statements and expressions begun, which pace the clock and memory
    /// checks of [`Self::over_budget`].
    polls: u32,
}

/// Expression types by node, recorded or replayed.
#[derive(Default)]
pub(crate) struct Memo {
    types: HashMap<usize, ty::Ty>,
    replay: bool,
}

/// The name of a symbol literal's value.
fn symbol_text(value: &crate::Value) -> Option<String> {
    match &value.0 {
        crate::value::Kind::Symbol(symbol) => {
            Some(String::from_utf8_lossy(&symbol.data).into_owned())
        }
        _ => None,
    }
}
