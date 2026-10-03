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
use counted::{CountedMap, CountedSet, CountedVec, Ledger};
use std::{collections::HashMap, fmt};

/// A diagnostic's text, written as `format!` writes it, through the
/// checker's meter ([`counted::text`]).
macro_rules! text {
    ($checker:expr, $($arg:tt)*) => {
        $checker.text(format_args!($($arg)*))
    };
}

mod assigns;
mod calls;
mod check;
mod construction;
mod counted;
mod expr;
mod flow;
mod foreign;
mod marks;
mod meter;
pub(crate) use meter::Heap;
mod modules;
mod program;
mod sigs;
mod spans;
mod ty;
mod walk;

/// What the checker reads: one source, parsed, with the host functions and
/// capabilities its engine registers.
pub(crate) struct Input<'a> {
    pub source: &'a str,
    pub parsed: &'a Declarations,
    /// The tokens the parser read, for exact spans.
    pub tokens: &'a [crate::tooling::Token],
    pub hosts: &'a [(&'a String, &'a Registered)],
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
    /// Told what the check does as it goes, by a test measuring its
    /// memory.
    pub observe: Option<fn(Observed)>,
    /// Whether to write out the types of a host script's top-level locals
    /// and of its result, which a session continuing it declares and a
    /// compilation never reads.
    pub annotate: bool,
}

/// What a check tells a test observing it.
#[doc(hidden)]
#[derive(Clone, Copy, Debug)]
pub enum Observed {
    /// The source is parsed, and the checker is about to start.
    Checking,
    /// The checker measured what it holds by its account: `held` stays,
    /// and `peak` adds what an operation holds for a moment beside it.
    Measured { held: usize, peak: usize },
    /// The checker is done, and the pass over the canonical surface is
    /// about to start.
    Surfacing,
    /// The check is done, and the parsed source is about to be dropped.
    Checked,
}

/// Resolves a required module's name to its source and filename, charging
/// the search and the read to the context given, which the check's budget
/// bounds.
pub(crate) type Modules<'a> = dyn Fn(
        &str,
        Option<&crate::loading::Origin>,
        &mut crate::CallContext,
    ) -> crate::Result<(String, crate::loading::Origin)>
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
    /// a value, or it takes more than 16 KiB to spell out. A host
    /// continuing a session, as `vibes repl` does, declares them for the
    /// next script.
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
    /// The most memory the checker held while checking, by its own account
    /// of its tables, which a memory quota bounds.
    pub peak_bytes: usize,
    /// About the most memory the pass over the canonical surface held
    /// beside the checker's tables, which the quota bounds with them; 0
    /// when it did not run.
    pub surface_bytes: usize,
    /// What the checker's tables held, by its account, while the pass over
    /// the canonical surface ran beside them; 0 when it did not run.
    pub(crate) surfaced: usize,
    /// What the sources and file names the diagnostics of required files
    /// keep hold, each counted once however many keep it, which last as
    /// long as the diagnostics do.
    pub(crate) retained: usize,
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
    bases: CountedMap<usize, Option<crate::members::direct::Base>>,
    /// Whether each expression's value is plain: it can hold no host method
    /// or exported function, which only values of type `any`, capabilities
    /// and required modules can. The runtime skips its scan for them. Also
    /// records a single numeric type, when proved, for arithmetic dispatch.
    values: CountedMap<usize, ValueFact>,
    /// Whether every parameter of each block is plain.
    blocks: CountedMap<usize, bool>,
    /// For each member call whose receiver is always an instance of one
    /// class this source declares, and whose member is that class's method,
    /// the class's qualified name; `None` where checks disagreed.
    classes: CountedMap<usize, Option<String>>,
    /// The instance methods whose result the checker proves, by the offset
    /// of their definition: no method of their class can read an instance
    /// variable before it is assigned ([`construction`]). The compiler moves
    /// definitions, so their offset names them.
    results: CountedSet<u32>,
    /// What the recorded class names hold, for the checker's memory
    /// account.
    names: usize,
}

/// A numeric type proved by the checker. Integers may use compact or big storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Number {
    Int,
    Float,
}

impl counted::Owned for ValueFact {
    fn owned(&self) -> usize {
        0
    }
}

#[derive(Clone, Copy, Debug)]
struct ValueFact {
    plain: bool,
    number: Option<Number>,
}

// A fact the budget refuses room for is not kept: the check has stopped,
// and the compiler reads a missing fact as proving nothing.
impl Facts {
    fn record_base(
        &mut self,
        tables: Ledger<'_>,
        call: &crate::syntax::Expr,
        base: Option<crate::members::direct::Base>,
    ) {
        if let Ok(recorded) = self.bases.get_or_insert_with(tables, key(call), || base) {
            if *recorded != base {
                *recorded = None;
            }
        }
    }

    fn record_value(
        &mut self,
        tables: Ledger<'_>,
        expr: &crate::syntax::Expr,
        plain: bool,
        number: Option<Number>,
    ) {
        let fact = ValueFact { plain, number };
        if let Ok(recorded) = self.values.get_or_insert_with(tables, key(expr), || fact) {
            recorded.plain &= plain;
            if recorded.number != number {
                recorded.number = None;
            }
        }
    }

    /// Records the class whose method `call` calls, copying its name the
    /// first time, counted before it is.
    fn record_class(
        &mut self,
        tables: Ledger<'_>,
        call: &crate::syntax::Expr,
        class: Option<&str>,
    ) {
        if let Some(recorded) = self.classes.get_mut(&key(call)) {
            if recorded.as_deref() != class {
                *recorded = None;
            }
            return;
        }
        let name = class.map_or(0, str::len);
        let Ok(mut kept) = tables.keep(name) else {
            return;
        };
        if self.classes.reserve(tables, 1).is_err() {
            return;
        }
        let class = class.map(str::to_owned);
        self.names += class.as_ref().map_or(0, String::capacity);
        self.classes.insert_kept(&mut kept, key(call), class);
    }

    /// Records that the checker proves `def`'s result; whether it kept
    /// that.
    fn record_result(&mut self, tables: Ledger<'_>, def: &crate::syntax::Definition) -> bool {
        self.results.insert(tables, def.offset).is_ok()
    }

    fn record_block(&mut self, tables: Ledger<'_>, block: &crate::syntax::Block, plain: bool) {
        if let Ok(recorded) = self.blocks.get_or_insert_with(tables, key(block), || plain) {
            *recorded &= plain;
        }
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

    /// What the facts' tables hold.
    fn bytes(&self) -> usize {
        use meter::{map, set};
        map(&self.bases)
            + map(&self.values)
            + map(&self.blocks)
            + map(&self.classes)
            + set(&self.results)
            + self.names
    }
}

/// A syntax node's identity, which is stable from checking to compiling.
fn key<T>(node: &T) -> usize {
    std::ptr::from_ref(node) as usize
}

impl Checked {
    /// The most memory the check held at once, by its account: the
    /// checker's peak, or what its tables held while the pass over the
    /// canonical surface ran beside them with the most the pass held,
    /// whichever is more. The checker's peak may pass before the pass
    /// starts, so the two are not held at once.
    pub(crate) fn peak(&self) -> usize {
        self.peak_bytes
            .max(self.surfaced.saturating_add(self.surface_bytes))
    }

    /// The diagnostics and the exports, which a check that requires the
    /// file keeps or imports, letting go of the rest of what it found.
    pub(crate) fn into_kept(
        self,
    ) -> (
        Vec<Diagnostic>,
        Option<std::sync::Arc<modules::Exported>>,
        usize,
    ) {
        (self.diagnostics, self.exported, self.retained)
    }

    /// What the check's findings hold, which last while the compiler reads
    /// them: the facts, the diagnostics with the sources and file names
    /// they keep, the call types and the locals.
    pub(crate) fn bytes(&self) -> usize {
        use meter::Heap;
        let calls = self.calls.entries.capacity() * size_of::<(usize, ReceiverType)>()
            + self
                .calls
                .entries
                .iter()
                .map(|(_, call)| call.heap())
                .sum::<usize>();
        self.facts.bytes() + self.diagnostics.heap() + calls + self.locals.heap() + self.retained
    }
}

/// What checking the top-level statements of a host script found, for
/// [`Checked::locals`] and [`Checked::result`].
pub(crate) struct Session {
    pub(crate) locals: Vec<(String, ty::Ty)>,
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

    /// The calls `entries` records, in order of their offsets: the first
    /// recorded at an offset is kept.
    pub(crate) fn from_entries(mut entries: Vec<(usize, ReceiverType)>) -> Self {
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

/// The most items of a list a diagnostic names; it counts the others.
const LISTED: usize = 20;

/// The `items` a diagnostic lists, each written by `write` after a `, `,
/// up to [`LISTED`] of them and then how many more there are, as in
/// `:a, :b and 3 more`, and how many there are in all. The items past
/// those it names are only counted, so a list of any length costs one
/// short string.
fn listed<T>(
    meter: &std::sync::Arc<meter::Meter>,
    items: impl IntoIterator<Item = T>,
    mut write: impl FnMut(&mut counted::Text<'_>, T),
) -> (counted::HeldText, usize) {
    // Written through the meter, since the items named can be long.
    let mut out = counted::Text::new(meter);
    let mut count = 0;
    let mut items = items.into_iter();
    loop {
        if meter.charge(1) {
            break;
        }
        let Some(item) = items.next() else {
            break;
        };
        if count < LISTED {
            if count > 0 {
                out.push_str(", ");
            }
            write(&mut out, item);
            if meter.stopped() {
                break;
            }
        }
        count += 1;
    }
    if count > LISTED {
        out.write(format_args!(" and {} more", count - LISTED));
    }
    (out.finish_held(meter), count)
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
    let meter = meter::Meter::new(input.budget.clone(), input.observe);
    // The spans may copy the tokens, before the type table first polls.
    let spans = spans::Spans::new(
        input.source,
        input.tokens,
        &input.parsed.interpolations,
        std::sync::Arc::clone(&meter),
    );
    // What the check holds from the start, which the tables' growth is
    // then counted beside.
    meter.outside(spans.bytes());
    meter.held(0);
    // Parsing the interpolations again can use up the budget, and then
    // nothing else is checked.
    if meter.stopped() {
        return Checked {
            steps: meter.steps(),
            stopped: true,
            peak_bytes: meter.peak(),
            ..Checked::default()
        };
    }
    let mut checker = Checker {
        source: input.source,
        parsed: input.parsed,
        spans,
        types: ty::Types::metered(std::sync::Arc::clone(&meter)),
        program: program::Program::default(),
        converter: sigs::Converter::default(),
        diagnostics: CountedVec::new(),
        calls: CountedVec::new(),
        constants: CountedMap::new(),
        frame: check::Frame::new(&meter, None, false, None, String::new()),
        purposes: counted::ScratchVec::new(&meter),
        mute: 0,
        modules: modules::Required::new(input, depth),
        memo: meter::MemoSlot::default(),
        write_chain: CountedSet::new(),
        fetch_receivers: CountedMap::new(),
        session: None,
        annotate: input.annotate,
        too_deep: false,
        facts: Facts::default(),
        construction: construction::Construction::default(),
        self_receiver: false,
        symbols_stay: None,
        annotating: 0,
        storing_self: None,
        assigns: assigns::Assigns::default(),
        meter: std::sync::Arc::clone(&meter),
        stopped: false,
        grown: 0,
        declared_bytes: 0,
        declared_count: 0,
        saved: 0,
        scratch: 0,
        defaults: None,
    };
    for (name, host) in input.hosts {
        let function = crate::signatures::host::function(name, host);
        let sig = std::rc::Rc::new(
            checker
                .converter
                .convert_owned(&mut checker.types, &function, None)
                .host(),
        );
        // Each signature is counted before it is kept, and its name as the
        // table takes it; a check they stop registers no more.
        let declarations = meter.declarations();
        if declarations.keep(meter::Heap::heap(&sig)).is_err()
            || checker
                .program
                .hosts
                .insert_made(declarations, name.len(), || (*name).clone(), sig)
                .is_err()
        {
            break;
        }
    }
    checker.declare_hosts(input.declared);
    checker.program.file = input.file;
    checker.declare_program(input.parsed);
    // A check that ran out of its budget declaring the program checks none
    // of it.
    if !checker.halted() {
        checker.check_retained_declarations(input.declared, input.parsed);
        if !checker.declared() {
            checker.check_all();
        }
    }
    checker.held();
    // A required file's exports copy its public declarations beside them,
    // which the budget bounds with the rest before they are made.
    let copies = if input.file && !checker.halted() {
        checker.export_bytes()
    } else {
        0
    };
    let stopped = checker.halted() || meter.scratch_lists().keep(copies).is_err();
    // A check past its budget exports nothing: its caller stops too.
    let exported = (input.file && !stopped).then(|| checker.export().map(std::sync::Arc::new));
    // A sort an export refuses stops the check, which exports nothing.
    let exported = exported.flatten().filter(|_| !meter.stopped());
    let stopped = stopped || meter.stopped();
    let (mut locals, result, stopped) = match checker.session.take().filter(|_| input.annotate) {
        Some(session) if !stopped => annotated(&mut checker.types, &meter, session),
        _ => (Vec::new(), None, stopped),
    };
    let stopped = stopped || counted::sort_unstable_by(&meter, &mut locals, Ord::cmp).is_err();
    let too_deep = checker.too_deep;
    let mut diagnostics = checker.diagnostics.into_vec();
    let mut calls = checker.calls.into_vec();
    // The diagnostics and the calls are put in order, keeping those of an
    // offset in the order they were found; a check past its budget fails,
    // whatever it found, so what it found is dropped rather than put in
    // order, which copies it.
    let in_order = |a: &Diagnostic, b: &Diagnostic| {
        (a.span.start, a.span.end).cmp(&(b.span.start, b.span.end))
    };
    let stopped = stopped
        || counted::sort_by(&meter, &mut diagnostics, in_order).is_err()
        || counted::sort_by(&meter, &mut calls, |a, b| a.0.cmp(&b.0)).is_err();
    if stopped {
        diagnostics = Vec::new();
        calls = Vec::new();
    } else {
        diagnostics.dedup_by(|a, b| a.code == b.code && a.span == b.span && a.message == b.message);
    }
    let mut checked = Checked {
        diagnostics,
        calls: CallTypes::from_entries(calls),
        // Every step charged, the exports', the annotations' and the sorts'
        // with the check's.
        steps: meter.steps(),
        exported,
        locals,
        result,
        facts: checker.facts,
        stopped,
        peak_bytes: meter.peak(),
        surface_bytes: 0,
        surfaced: 0,
        // A check that stopped keeps no diagnostics.
        retained: if stopped { 0 } else { checker.modules.kept },
    };
    // Removed spellings of the canonical surface are compile errors too,
    // unless the source is too tall to walk or the check stopped early.
    if !too_deep && !stopped {
        // The pass charges a step a token up front, those it lexes again
        // inside interpolations too, and each entry of a percent literal
        // it rewrites, and holds a tree of the tokens beside what the
        // checker keeps, and for a moment the syntax of each interpolation
        // it parses again.
        let interpolated = input.parsed.interpolated;
        let Some((tokens, surface)) = surface_setup(
            &meter,
            input.tokens,
            interpolated,
            checker.program.namespaces.iter().map(|ns| ns.name.len()),
        ) else {
            checked.stopped = true;
            checked.steps = meter.steps();
            return checked;
        };
        checked.steps = meter.steps();
        let surface = surface + checker.spans.parsing();
        // A required file's exports hold its type table, which the pass
        // runs beside, and copies of its public declarations; with them the
        // check holds what its peak, which then counts them, holds at least.
        let exported = checked
            .exported
            .as_ref()
            .map_or(0, |exported| exported.types_bytes());
        let checker_held = meter.held(checker.types.bytes()) + exported;
        checked.peak_bytes = checked.peak_bytes.max(checker_held);
        let held = checker_held + surface;
        if input
            .budget
            .steps
            .is_some_and(|left| checked.steps + tokens > left)
        {
            // Compilation fails charging them, and a check that requires
            // this file stops with it rather than import what it exports.
            checked.steps += tokens;
            meter.stop();
            checked.stopped = true;
        } else if input.budget.memory.is_some_and(|left| held > left) {
            meter.stop();
            checked.stopped = true;
        } else {
            if let Some(observe) = input.observe {
                observe(Observed::Surfacing);
            }
            checked.surface_bytes = surface;
            checked.surfaced = checker_held;
            let budget = &input.budget;
            let asked = std::sync::atomic::AtomicU32::new(0);
            crate::surface::add_to(
                &mut checked,
                input.source,
                input.tokens,
                interpolated.tokens + interpolated.entries,
                &|steps| {
                    // The pass asks at every statement and expression it
                    // visits: the cancellation token each time, and the
                    // deadline, whose clock costs more, every 64th.
                    let cancelled = budget
                        .cancellation
                        .as_ref()
                        .is_some_and(crate::CancellationToken::is_cancelled);
                    let count = asked.load(std::sync::atomic::Ordering::Relaxed);
                    asked.store(count.wrapping_add(1), std::sync::atomic::Ordering::Relaxed);
                    let late = count % 64 == 63
                        && budget
                            .deadline
                            .is_some_and(|deadline| std::time::Instant::now() >= deadline);
                    let within =
                        !(budget.steps.is_some_and(|left| steps > left) || cancelled || late);
                    if !within {
                        meter.stop();
                    }
                    within
                },
                // The compiler's parse of a source the rules cannot read,
                // charged in advance with the pass, runs within the steps
                // and memory the budget leaves, and stops the check if it
                // runs out of them.
                &|source, steps| {
                    let mut context = crate::CallContext::new(crate::CallOptions {
                        limits: crate::Limits {
                            steps: budget.steps.map(|left| left.saturating_sub(steps)),
                            memory_bytes: budget.memory.map(|left| left.saturating_sub(held)),
                            ..crate::Limits::default()
                        },
                        cancellation: budget.cancellation.clone().unwrap_or_default(),
                        deadline: budget.deadline,
                        ..crate::CallOptions::default()
                    });
                    let error = crate::syntax::canonical_error(
                        source,
                        &crate::compilation::Meter(std::cell::RefCell::new(&mut context)),
                    );
                    if context.exhausted() {
                        meter.stop();
                        return None;
                    }
                    Some(error)
                },
                // The text its fixes copy and render may take what the
                // memory leaves beside the check and the pass's footprint;
                // more stops the check.
                budget.memory.map(|left| left.saturating_sub(held)),
            );
            if checked.stopped {
                meter.stop();
            }
        }
    }
    checked
}

/// Counts surface metadata without an uninterruptible pass over token payloads.
fn surface_setup(
    meter: &meter::Meter,
    tokens: &[crate::tooling::Token],
    mut totals: crate::syntax::Interpolated,
    names: impl Iterator<Item = usize>,
) -> Option<(u64, usize)> {
    use crate::tooling::TokenKind;
    use meter::Heap;
    for (at, token) in tokens.iter().enumerate() {
        if meter.pace(1, 0) {
            return None;
        }
        // A string that labels a hash pair is copied again as the pair's
        // name, beside the token's copy.
        if let TokenKind::String(bytes) = &token.kind {
            if tokens
                .get(at + 1)
                .is_some_and(|next| next.kind == TokenKind::Punct(':'))
            {
                totals.bytes += bytes.len();
            }
        }
        totals.tokens += 1;
        // One token's entries, which this loop paces.
        totals.entries +=
            crate::surface::entries(std::slice::from_ref(token), &|| false).unwrap_or(0);
        match &token.kind {
            TokenKind::Words { entries, .. } => {
                totals.bytes += entries.capacity() * size_of::<Option<Vec<u8>>>();
                for entry in entries {
                    if meter.charge(1) {
                        return None;
                    }
                    if let Some(entry) = entry {
                        totals.bytes += entry.capacity();
                        totals.rewritten += entry.len();
                    }
                }
            }
            _ => totals.bytes += token.heap(),
        }
        if token.kind == TokenKind::Word {
            totals.words += token.span.len();
        }
    }
    let mut qualified = 0;
    for length in names {
        if meter.charge(1) {
            return None;
        }
        qualified += length;
    }
    Some((
        (totals.tokens + totals.entries) as u64,
        // The totals are counted above; no token is left to pace.
        crate::surface::footprint(&[], totals, qualified, &|| false)?,
    ))
}

/// Checks that the command line can call `function` with `count` arguments,
/// which it passes as strings: each positional parameter they bind must
/// accept `string`, and a rest parameter `array<string>`.
pub(crate) fn entry_arguments(input: &Input<'_>, function: &str, count: usize) -> Vec<Diagnostic> {
    let meter = meter::Meter::new(input.budget.clone(), input.observe);
    // The spans may copy the tokens, before the type table first polls.
    let spans = spans::Spans::new(
        input.source,
        input.tokens,
        &input.parsed.interpolations,
        std::sync::Arc::clone(&meter),
    );
    meter.outside(spans.bytes());
    let mut checker = Checker {
        source: input.source,
        parsed: input.parsed,
        spans,
        types: ty::Types::metered(std::sync::Arc::clone(&meter)),
        program: program::Program::default(),
        converter: sigs::Converter::default(),
        diagnostics: CountedVec::new(),
        calls: CountedVec::new(),
        constants: CountedMap::new(),
        frame: check::Frame::new(&meter, None, false, None, String::new()),
        purposes: counted::ScratchVec::new(&meter),
        mute: 0,
        modules: modules::Required::new(input, 0),
        memo: meter::MemoSlot::default(),
        write_chain: CountedSet::new(),
        fetch_receivers: CountedMap::new(),
        session: None,
        annotate: false,
        too_deep: false,
        facts: Facts::default(),
        construction: construction::Construction::default(),
        self_receiver: false,
        symbols_stay: None,
        annotating: 0,
        storing_self: None,
        assigns: assigns::Assigns::default(),
        meter: std::sync::Arc::clone(&meter),
        stopped: false,
        grown: 0,
        declared_bytes: 0,
        declared_count: 0,
        saved: 0,
        scratch: 0,
        defaults: None,
    };
    checker.declare_hosts(input.declared);
    checker.declare_program(input.parsed);
    checker.check_retained_declarations(input.declared, input.parsed);
    checker.diagnostics.clear();
    checker.entry_arguments(function, count);
    checker.diagnostics.into_vec()
}

/// The session's locals with their annotations, and the annotation of its
/// result, written out after the last measure: each type's annotation is
/// written once, and counted with those before it as it is, and the
/// locals' copies of them are counted before they are made. Returns
/// whether the check has stopped, which keeps none of them.
fn annotated(
    types: &mut ty::Types,
    meter: &std::sync::Arc<meter::Meter>,
    session: Session,
) -> (Vec<(String, String)>, Option<String>, bool) {
    let stopped = (Vec::new(), None, true);
    // Each type's annotation, written once, in a map counted, with the
    // text, while it lives.
    let mut written = counted::ScratchMap::new(meter);
    for &(_, ty) in &session.locals {
        if written.contains_key(&ty) {
            continue;
        }
        let text = types.annotation(ty);
        if written.insert(ty, text).is_err() {
            return stopped;
        }
    }
    let copies = session.locals.len() * size_of::<(String, String)>()
        + session
            .locals
            .iter()
            .map(|(name, ty)| name.capacity() + written[ty].len())
            .sum::<usize>();
    if meter.scratch(copies) {
        return stopped;
    }
    let result = types.annotation(session.result);
    let mut locals = Vec::with_capacity(session.locals.len());
    locals.extend(
        session
            .locals
            .into_iter()
            .map(|(name, ty)| (name, written[&ty].clone())),
    );
    (locals, Some(result), false)
}

/// The state of one check.
pub(crate) struct Checker<'a> {
    source: &'a str,
    parsed: &'a Declarations,
    spans: spans::Spans<'a>,
    types: ty::Types,
    program: program::Program<'a>,
    converter: sigs::Converter,
    diagnostics: CountedVec<Diagnostic>,
    calls: CountedVec<(usize, ReceiverType)>,
    /// Constants of class and module bodies, by namespace and name.
    constants: CountedMap<(Option<program::NsId>, String), ty::Ty>,
    frame: check::Frame,
    /// Why the value being checked against a type is checked, innermost last.
    purposes: counted::ScratchVec<check::Purpose>,
    /// While positive, diagnostics are dropped: a second look at code that
    /// was already checked.
    mute: u32,
    /// The modules the program requires, and the names they publish.
    modules: modules::Required<'a>,
    /// Expression types recorded while checking a call on one alternative
    /// of a union receiver, which the other alternatives replay.
    memo: meter::MemoSlot,
    /// The reads a write goes through, by node: the receivers of an index
    /// or member assignment's target and of a mutating call, down to their
    /// root. An index among them reads its element as present, since the
    /// runtime raises when it is missing, and no fix rewrites one, since a
    /// write through a rewritten read would reach a copy.
    write_chain: CountedSet<usize>,
    /// Receiver types of optional indexed reads, for diagnostic fixes only.
    /// None when repeated checks disagree about the receiver.
    fetch_receivers: CountedMap<usize, Option<ty::Ty>>,
    /// The top-level statements' locals and result, once checked.
    session: Option<Session>,
    /// Whether to keep what a session continuing a host script declares.
    annotate: bool,
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
    /// How many levels of an annotation the checker is resolving, through
    /// its parts and the aliases it names.
    annotating: u32,
    /// The instance variable that `self` itself is being stored into, as in
    /// `@next = self`, which the store assigns rather than lets escape.
    storing_self: Option<String>,
    /// The names each `begin`, loop and block body assigns.
    assigns: assigns::Assigns<'a>,
    /// The check's work and memory account, and whether it stopped at its
    /// budget.
    meter: std::sync::Arc<meter::Meter>,
    stopped: bool,
    /// What tables that grow an element at a time hold beyond their own
    /// storage: diagnostics, member call types, constants' names and the
    /// classes facts record.
    grown: usize,
    /// What the program's declarations hold, measured when they change.
    declared_bytes: usize,
    /// How many functions, classes, modules and enums there were when the
    /// declarations were last measured.
    declared_count: usize,
    /// What the frames that enclosing checks set aside hold.
    saved: usize,
    /// What the operations under way keep beside the tables while they
    /// check more code: [`Self::hold`] adds to it and [`Self::release`]
    /// takes it back.
    scratch: usize,
    /// Each class's instance-variable defaults, by the class's offset,
    /// with the variable each assigns: gathered in one pass the first time
    /// a class body needs them, and taken as each is checked.
    defaults: Option<Defaults<'a>>,
}

/// Instance-variable defaults by their class's offset, with the variable
/// each assigns, and what the table holds.
type Defaults<'a> = (
    HashMap<u32, Vec<(&'a crate::syntax::Stmt, Option<&'a str>)>>,
    usize,
);

/// Expression types by node, recorded or replayed.
#[derive(Default)]
pub(crate) struct Memo {
    types: CountedMap<usize, ty::Ty>,
    replay: bool,
}

impl Memo {
    /// What the recorded types take.
    fn bytes(&self) -> usize {
        meter::map(&self.types)
    }
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

#[cfg(test)]
mod budget_review_tests {
    use super::*;
    use std::cell::Cell;

    #[test]
    fn diagnostic_list_stays_charged_while_its_message_is_rendered() {
        let meter = meter::Meter::new(
            crate::compilation::Budget {
                memory: Some(1_536),
                ..Default::default()
            },
            None,
        );
        let name = "a".repeat(1_024);
        let (names, count) = listed(&meter, [&name], |out, name| out.push_str(name));
        assert_eq!(count, 1);
        assert_eq!(&*names, name);
        assert_eq!(meter.unmeasured(), 1_024);
        assert_eq!(meter.held(0), 1_024);
        assert_eq!(meter.unmeasured(), 1_024);
        let message = counted::text(&meter, format_args!("{names}"));
        assert!(message.is_empty());
        assert!(meter.stopped());
        assert_eq!(meter.unmeasured(), 1_024);
        drop(names);
        assert_eq!(meter.unmeasured(), 0);
    }

    #[test]
    fn refused_diagnostic_list_releases_its_partial_text() {
        let meter = meter::Meter::new(
            crate::compilation::Budget {
                memory: Some(32),
                ..Default::default()
            },
            None,
        );
        let (names, _) = listed(
            &meter,
            ["short", "a name longer than the remaining quota"],
            |out, name| {
                out.push_str(name);
            },
        );
        assert!(names.is_empty());
        assert!(meter.stopped());
        assert_eq!(meter.unmeasured(), 0);
        drop(names);
        assert_eq!(meter.unmeasured(), 0);
    }

    #[test]
    fn diagnostic_list_stops_counting_when_its_budget_stops() {
        let meter = meter::Meter::new(
            crate::compilation::Budget {
                steps: Some(32),
                ..Default::default()
            },
            None,
        );
        let visited = Cell::new(0);
        listed(
            &meter,
            (0..100_000).inspect(|_| visited.set(visited.get() + 1)),
            |_, _| {},
        );
        assert!(meter.stopped());
        assert!(visited.get() <= 34, "visited {} items", visited.get());
    }

    #[test]
    fn diagnostic_list_stops_after_a_refused_name() {
        let meter = meter::Meter::new(
            crate::compilation::Budget {
                memory: Some(16),
                ..Default::default()
            },
            None,
        );
        let visited = Cell::new(0);
        listed(
            &meter,
            (0..100_000).inspect(|_| visited.set(visited.get() + 1)),
            |out, _| {
                out.push_str("this name exceeds the entire memory budget");
            },
        );
        assert!(meter.stopped());
        assert_eq!(visited.get(), 1);
    }
}
