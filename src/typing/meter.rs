//! The checker's account of its own work and memory, which one budget
//! bounds.
//!
//! Every part of the checker charges its work to one running total of
//! steps: the type table directly, and statements, flow and spans through
//! counters the checker drains into the total whenever it polls. The
//! memory account is the size of every table the checker owns, counting
//! what their elements hold on the heap: [`Heap`] measures a table, and
//! tables that grow one element at a time keep a running count of their
//! elements' payloads instead of being walked at each poll. Each poll
//! compares both totals with the budget and records the peak.

use crate::compilation::Budget;
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    mem::size_of,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed},
    },
};

/// The memo of expression types the checker records or replays, which
/// only [`super::Checker::set_memo`] and the restores that undo it swap,
/// so a memo set aside stays in the account.
#[derive(Default)]
pub(crate) struct MemoSlot(Option<super::Memo>);

impl MemoSlot {
    pub fn get(&self) -> Option<&super::Memo> {
        self.0.as_ref()
    }

    pub fn get_mut(&mut self) -> Option<&mut super::Memo> {
        self.0.as_mut()
    }
}

/// The running totals, which the checker and its type table share.
#[derive(Debug, Default)]
pub(crate) struct Meter {
    budget: Budget,
    steps: AtomicU64,
    /// The bytes the checker's tables other than the type table held when
    /// it last polled, which the type table adds its own to.
    outside: AtomicUsize,
    /// What the check held when it was last measured.
    last: AtomicUsize,
    peak: AtomicUsize,
    stopped: AtomicBool,
    polls: AtomicU64,
    /// The steps charged when a poll or a charge last checked the deadline
    /// and the cancellation token.
    asked: AtomicU64,
    /// Told of every measure, by a test comparing the account with what
    /// the check really holds.
    observe: Option<fn(super::Observed)>,
}

impl Meter {
    pub fn new(budget: Budget, observe: Option<fn(super::Observed)>) -> Arc<Self> {
        Arc::new(Self {
            budget,
            observe,
            ..Self::default()
        })
    }

    /// Adds `steps` of work to the total, and stops the check once the
    /// total passes the steps its budget leaves. Every [`INTERRUPTIBLE`]
    /// steps it also asks the deadline and the cancellation token, so work
    /// that charges without polling still stops for them. Returns whether
    /// the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub fn charge(&self, steps: u64) -> bool {
        let total = self.steps.fetch_add(steps, Relaxed).saturating_add(steps);
        if self.budget.steps.is_some_and(|left| total > left) {
            self.stop();
        } else if total.saturating_sub(self.asked.load(Relaxed)) >= INTERRUPTIBLE {
            self.asked.store(total, Relaxed);
            if self.budget.interrupted() {
                self.stop();
            }
        }
        self.stopped()
    }

    /// The work charged so far.
    pub fn steps(&self) -> u64 {
        self.steps.load(Relaxed)
    }

    /// The most bytes an account has reported.
    pub fn peak(&self) -> usize {
        self.peak.load(Relaxed)
    }

    pub fn stopped(&self) -> bool {
        self.stopped.load(Relaxed)
    }

    pub fn stop(&self) {
        if !self.stopped.swap(true, Relaxed) {
            crate::budget::tripped();
        }
    }

    /// What the check may spend.
    pub fn budget(&self) -> &Budget {
        &self.budget
    }

    /// Records the checker's tables other than the type table.
    pub fn outside(&self, bytes: usize) {
        self.outside.store(bytes, Relaxed);
    }

    /// Records the type table's `bytes`, with the rest the checker last
    /// reported, as the memory held now.
    pub fn held(&self, types: usize) -> usize {
        let held = self.account(types);
        self.reach(held);
        held
    }

    /// The type table's `bytes` with the rest the checker last reported,
    /// kept as what the check held when last measured.
    fn account(&self, types: usize) -> usize {
        let held = types + self.outside.load(Relaxed);
        self.last.store(held, Relaxed);
        held
    }

    /// Records `extra` bytes of scratch an operation holds for a moment
    /// beside what the check held when last measured, stopping it if they
    /// pass the memory left; less than [`SCRATCH`] stays within the
    /// account's margin. Returns whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub fn scratch(&self, extra: usize) -> bool {
        if extra >= SCRATCH {
            let held = self.last.load(Relaxed) + extra;
            self.reach(held);
            if self.budget.memory.is_some_and(|left| held > left) {
                self.stop();
            }
        }
        self.stopped()
    }

    /// Charges `steps` of a walk over the syntax that holds `extra` bytes
    /// beside what the check held when last measured, and checks the
    /// steps, the deadline, the cancellation token and the memory against
    /// the budget. Returns whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub fn pace(&self, steps: u64, extra: usize) -> bool {
        if self.charge(steps) || self.budget.interrupted() {
            self.stop();
            return true;
        }
        self.scratch(extra)
    }

    /// Records that the check held `bytes` at some point, as while a
    /// required file was checked.
    pub fn reach(&self, bytes: usize) {
        self.peak.fetch_max(bytes, Relaxed);
        if let Some(observe) = self.observe {
            let held = self.last.load(Relaxed);
            observe(super::Observed::Measured {
                held: held.min(bytes),
                peak: bytes,
            });
        }
    }

    /// Records the type table's `bytes` with `extra` an operation holds
    /// while it runs, stopping the check if they pass the memory left.
    /// Returns whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub fn transient(&self, types: usize, extra: usize) -> bool {
        let held = self.account(types) + extra;
        self.reach(held);
        if self.budget.memory.is_some_and(|left| held > left) {
            self.stop();
        }
        self.stopped()
    }

    /// Checks the totals against the budget: the steps on every poll, and
    /// on every 64th the memory `measure` reports, the deadline and the
    /// cancellation token, whose checks cost more. The deadline and the
    /// cancellation token are also checked once [`INTERRUPTIBLE`] steps of
    /// work have passed since they last were, however few polls that took.
    /// Once past any of them the check stays stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub fn poll(&self, measure: impl FnOnce() -> usize) -> bool {
        if self.stopped() {
            return true;
        }
        let steps = self.steps();
        if self.budget.steps.is_some_and(|left| steps > left) {
            self.stop();
            return true;
        }
        if self.polls.fetch_add(1, Relaxed) % 64 == 0 {
            self.asked.store(steps, Relaxed);
            let held = measure();
            if self.budget.interrupted() || self.budget.memory.is_some_and(|left| held > left) {
                self.stop();
            }
        } else if steps.saturating_sub(self.asked.load(Relaxed)) >= INTERRUPTIBLE {
            self.asked.store(steps, Relaxed);
            if self.budget.interrupted() {
                self.stop();
            }
        }
        self.stopped()
    }
}

/// What a value owns on the heap, beyond its own size.
pub(crate) trait Heap {
    fn heap(&self) -> usize;
}

/// The most steps of work a check does between checks of its deadline and
/// its cancellation token, which its charges make when its polls are far
/// apart.
const INTERRUPTIBLE: u64 = 1 << 12;

/// Scratch smaller than this is left to the account's margin rather than
/// checked against the memory left as soon as it is taken.
const SCRATCH: usize = 4096;

/// The bytes a hash table of `capacity` entries of type `T` allocates:
/// its buckets, a power of two that keeps it at most seven eighths full,
/// each with a control byte.
pub(crate) fn table<T>(capacity: usize) -> usize {
    if capacity == 0 {
        return 0;
    }
    let buckets = (capacity * 8 / 7).next_power_of_two().max(4);
    buckets * (size_of::<T>() + 1) + 16
}

macro_rules! plain {
    ($($t:ty),*) => {
        $(impl Heap for $t {
            fn heap(&self) -> usize {
                0
            }
        })*
    };
}

plain!(
    bool,
    u8,
    u32,
    u64,
    usize,
    i64,
    super::ty::Ty,
    super::flow::VarState,
    crate::diagnostic::Span
);

impl Heap for String {
    fn heap(&self) -> usize {
        self.capacity()
    }
}

impl Heap for str {
    fn heap(&self) -> usize {
        // Its bytes are the allocation of the box or string that holds it.
        0
    }
}

impl<T: ?Sized> Heap for &T {
    fn heap(&self) -> usize {
        // A reference owns nothing.
        0
    }
}

impl<T: Heap + ?Sized> Heap for Box<T> {
    fn heap(&self) -> usize {
        std::mem::size_of_val(&**self) + (**self).heap()
    }
}

impl<T: Heap> Heap for [T] {
    fn heap(&self) -> usize {
        self.iter().map(Heap::heap).sum()
    }
}

impl<T: Heap> Heap for Vec<T> {
    fn heap(&self) -> usize {
        self.capacity() * size_of::<T>() + self.iter().map(Heap::heap).sum::<usize>()
    }
}

impl<T: Heap> Heap for Option<T> {
    fn heap(&self) -> usize {
        self.as_ref().map_or(0, Heap::heap)
    }
}

impl<T: Heap> Heap for Rc<T> {
    fn heap(&self) -> usize {
        2 * size_of::<usize>() + size_of::<T>() + (**self).heap()
    }
}

impl<T: Heap> Heap for Arc<T> {
    fn heap(&self) -> usize {
        2 * size_of::<usize>() + size_of::<T>() + (**self).heap()
    }
}

impl<T: Heap, E: Heap> Heap for Result<T, E> {
    fn heap(&self) -> usize {
        match self {
            Ok(value) => value.heap(),
            Err(error) => error.heap(),
        }
    }
}

impl<A: Heap, B: Heap> Heap for (A, B) {
    fn heap(&self) -> usize {
        self.0.heap() + self.1.heap()
    }
}

impl<K: Heap, V: Heap, S> Heap for HashMap<K, V, S> {
    fn heap(&self) -> usize {
        table::<(K, V)>(self.capacity())
            + self
                .iter()
                .map(|(key, value)| key.heap() + value.heap())
                .sum::<usize>()
    }
}

impl<T: Heap, S> Heap for HashSet<T, S> {
    fn heap(&self) -> usize {
        table::<T>(self.capacity()) + self.iter().map(Heap::heap).sum::<usize>()
    }
}

/// The bytes of a B-tree node of `T`: up to eleven elements, with their
/// edges when it is internal.
fn btree_node<T>() -> usize {
    11 * size_of::<T>() + 12 * size_of::<usize>() + 16
}

impl<T: Heap> Heap for BTreeSet<T> {
    fn heap(&self) -> usize {
        // Every node but the root is at least half full.
        let nodes = if self.is_empty() {
            0
        } else {
            1 + self.len() / 5
        };
        nodes * btree_node::<T>() + self.iter().map(Heap::heap).sum::<usize>()
    }
}

/// What one more element takes in `set`, beside its own payload: the root
/// for the first, and a fifth of a node for each after it.
pub(crate) fn btree_entry<T>(set: &BTreeSet<T>) -> usize {
    if set.is_empty() {
        btree_node::<T>()
    } else {
        btree_node::<T>() / 5
    }
}

/// The bytes a table's own storage takes, without what its elements own,
/// for tables of elements that own nothing or whose payloads a running
/// count keeps.
pub(crate) fn vec<T>(items: &Vec<T>) -> usize {
    items.capacity() * size_of::<T>()
}

/// What a hash table of `bytes` holds beside itself while it grows: its
/// larger table is allocated before the old one, half the size, is freed.
/// An account adds this for the largest of its growing tables, since
/// only one grows at a time.
pub(crate) fn growth(bytes: usize) -> usize {
    bytes / 2
}

pub(crate) fn map<K, V, S>(entries: &HashMap<K, V, S>) -> usize {
    table::<(K, V)>(entries.capacity())
}

pub(crate) fn set<T, S>(entries: &HashSet<T, S>) -> usize {
    table::<T>(entries.capacity())
}

impl Heap for crate::diagnostic::Diagnostic {
    fn heap(&self) -> usize {
        self.message.heap()
            + self.labels.heap()
            + self.expected.heap()
            + self.found.heap()
            + self.fixes.heap()
    }
}

impl Heap for crate::diagnostic::Label {
    fn heap(&self) -> usize {
        self.message.heap()
    }
}

impl Heap for crate::diagnostic::Fix {
    fn heap(&self) -> usize {
        self.message.heap() + self.edits.heap()
    }
}

impl Heap for crate::diagnostic::Edit {
    fn heap(&self) -> usize {
        self.replacement.heap()
    }
}

impl Heap for crate::tooling::Token {
    fn heap(&self) -> usize {
        use crate::tooling::TokenKind;
        match &self.kind {
            TokenKind::String(bytes) | TokenKind::Symbol { name: bytes, .. } => bytes.capacity(),
            TokenKind::Template(parts) => parts.capacity() * size_of::<std::ops::Range<usize>>(),
            TokenKind::Words { entries, .. } => entries.heap(),
            _ => 0,
        }
    }
}

impl Heap for super::ReceiverType {
    fn heap(&self) -> usize {
        self.name.heap() + self.bases.heap()
    }
}

impl Heap for super::sigs::Sig {
    fn heap(&self) -> usize {
        self.name.heap() + self.params.heap() + self.block.heap() + self.vars.heap()
    }
}

impl Heap for super::sigs::Param {
    fn heap(&self) -> usize {
        self.name.heap()
    }
}

impl Heap for super::sigs::BlockSig {
    fn heap(&self) -> usize {
        self.params.heap()
    }
}

impl Heap for super::sigs::Var {
    fn heap(&self) -> usize {
        self.name.heap()
    }
}

impl<'a> super::Checker<'a> {
    /// The work charged so far.
    pub(super) fn total_steps(&self) -> u64 {
        self.meter.steps()
    }

    /// Whether the check must stop: its work passed the steps its budget
    /// leaves, its tables outgrew the memory left, the deadline passed or
    /// the host cancelled. Statements and expressions begun after that are
    /// skipped, and compilation fails with the budget's error. A check
    /// without a budget never stops.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub(super) fn over_budget(&mut self) -> bool {
        if !self.stopped {
            self.stopped = self.meter.poll(|| self.held());
        }
        self.stopped
    }

    /// Whether the check has stopped at its budget, as its own flag says or
    /// as the meter does, which a type operation or a measure can stop
    /// between the checker's polls.
    pub(super) fn halted(&self) -> bool {
        self.stopped || self.meter.stopped()
    }

    /// The memory the checker holds: its tables other than the type table,
    /// which the meter keeps for the type table's own polls, and the type
    /// table.
    pub(super) fn held(&self) -> usize {
        self.meter.outside(self.outside());
        self.meter.held(self.types.bytes())
    }

    /// Follows the declarations as they are made: measures them again, and
    /// records what the checker holds, once they grow by a quarter, so the
    /// account keeps up with them at a cost linear in their number.
    /// Returns whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub(super) fn declaring(&mut self) -> bool {
        let program = &self.program;
        let count = program.fns.len() + program.namespaces.len() + program.enums.len();
        if count > self.declared_count + self.declared_count / 4 {
            self.declared_count = count;
            return self.declared();
        }
        self.halted()
    }

    /// Measures the declarations once they change, which is rarely: the
    /// program's functions, classes, modules and enums, the host's, and
    /// their names in the type table. Returns whether the check has
    /// stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub(super) fn declared(&mut self) -> bool {
        self.declared_bytes = self.program.heap() + self.types.names.heap() + self.modules.heap();
        // Declarations with nothing to check can grow the program to any
        // size between the checker's polls, so each measure of them is
        // checked against the budget.
        self.check_memory(0) || self.over_budget()
    }

    /// What the checker's tables other than the type table hold.
    fn outside(&self) -> usize {
        let frame = &self.frame;
        let contexts: usize = frame.contexts.iter().map(Heap::heap).sum();
        let largest = [
            self.facts.largest(),
            self.spans.table(),
            self.assigns.largest(),
            map(&self.constants),
            set(&self.write_chain),
            map(&self.fetch_receivers),
            self.memo.get().map_or(0, super::Memo::bytes),
            self.construction.largest(),
        ]
        .into_iter()
        .max()
        .unwrap_or(0);
        growth(largest)
            + vec(&self.diagnostics)
            + vec(&self.calls)
            + map(&self.constants)
            + self.grown
            + self.declared_bytes
            + self.converter.grown()
            + self.spans.bytes()
            + frame.heap()
            + contexts
            + self.saved
            + self.scratch
            + self.purposes.heap()
            + self.memo.get().map_or(0, super::Memo::bytes)
            + set(&self.write_chain)
            + map(&self.fetch_receivers)
            + self.facts.bytes()
            + self.construction.bytes()
            + self.program.grown()
            + self.assigns.bytes()
    }

    /// Records `bytes` of scratch an operation holds beside the tables
    /// for a moment, such as the lists a walk over the syntax keeps, for
    /// the budget and the peak. Less than [`SCRATCH`] stays within the
    /// account's margin. Returns whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub(super) fn transient(&self, bytes: usize) -> bool {
        (bytes >= SCRATCH && self.check_memory(bytes)) || self.halted()
    }

    /// Counts `bytes` of scratch that an operation keeps while it checks
    /// more code, such as the types of a literal's elements, until
    /// [`Self::release`] takes them back; returns them. Many are checked
    /// against the memory left at once. A check that has stopped, or that
    /// they stop, holds nothing and gets `None`: the operation must not
    /// build what they count.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub(super) fn hold(&mut self, bytes: usize) -> Option<usize> {
        if self.halted() {
            return None;
        }
        self.scratch += bytes;
        if (bytes >= SCRATCH && self.check_memory(0)) || self.halted() {
            self.scratch -= bytes;
            return None;
        }
        Some(bytes)
    }

    /// Records that the check held `bytes` at some point, as while a
    /// required file was checked.
    pub(super) fn observed(&self, bytes: usize) {
        self.meter.reach(bytes);
    }

    /// Takes back scratch [`Self::hold`] counted.
    pub(super) fn release(&mut self, bytes: usize) {
        self.scratch -= bytes;
    }

    /// Records what the tables hold now with `extra` bytes beside them,
    /// stopping the check if they pass the memory left. Returns whether
    /// the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    fn check_memory(&self, extra: usize) -> bool {
        self.meter.outside(self.outside());
        self.meter.transient(self.types.bytes(), extra)
    }

    /// Makes `frame` current, setting the replaced one aside, which the
    /// account counts while it is.
    pub(super) fn enter_frame(&mut self, frame: super::check::Frame) -> super::check::Frame {
        let previous = std::mem::replace(&mut self.frame, frame);
        self.saved += previous.heap();
        previous
    }

    /// Restores a frame [`Self::enter_frame`] replaced.
    pub(super) fn leave_frame(&mut self, previous: super::check::Frame) {
        self.saved = self.saved.saturating_sub(previous.heap());
        self.frame = previous;
    }

    /// Makes `memo` current and returns the one it replaces, which the
    /// account counts while it is set aside.
    pub(super) fn set_memo(&mut self, memo: Option<super::Memo>) -> Option<super::Memo> {
        let aside = std::mem::replace(&mut self.memo.0, memo);
        self.saved += aside.as_ref().map_or(0, super::Memo::bytes);
        aside
    }

    /// Restores an enclosing memo [`Self::set_memo`] set aside, keeping
    /// what the inner one recorded when the enclosing one records too.
    pub(super) fn restore_memo(&mut self, outer: Option<super::Memo>) {
        let Some(mut inner) = self.put_back_memo(outer) else {
            return;
        };
        // A check past its budget keeps nothing more the inner one found.
        if self.halted() {
            return;
        }
        let Some(outer) = self.memo.0.as_mut().filter(|outer| !outer.replay) else {
            return;
        };
        // The smaller table moves into the larger, so a type recorded deep
        // in nested memos moves a logarithmic number of times rather than
        // once a level. The inner memo's types win.
        let swapped = inner.types.len() > outer.types.len();
        if swapped {
            std::mem::swap(&mut inner.types, &mut outer.types);
        }
        // Each type that moves is a step, and the table they move from is
        // live beside the tables until they have; a check they stop moves
        // none.
        if self.meter.charge(inner.types.len() as u64) || self.transient(inner.bytes()) {
            return;
        }
        let Some(outer) = self.memo.0.as_mut() else {
            return;
        };
        if swapped {
            for (key, ty) in inner.types {
                outer.types.entry(key).or_insert(ty);
            }
        } else {
            outer.types.extend(inner.types);
        }
    }

    /// Restores an enclosing memo [`Self::set_memo`] set aside, and
    /// returns the inner one.
    pub(super) fn put_back_memo(&mut self, outer: Option<super::Memo>) -> Option<super::Memo> {
        self.saved = self
            .saved
            .saturating_sub(outer.as_ref().map_or(0, super::Memo::bytes));
        std::mem::replace(&mut self.memo.0, outer)
    }

    /// Counts a diagnostic the checker keeps. Returns whether the check has
    /// stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub(super) fn keep(&mut self, diagnostic: &crate::diagnostic::Diagnostic) -> bool {
        self.grow(diagnostic.heap())
    }

    /// Measures the tables at once when one of them has grown by `bytes`,
    /// if that is enough to pass the account's margin, as a long name does.
    /// Returns whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub(super) fn grew(&self, bytes: usize) -> bool {
        (bytes >= SCRATCH && self.check_memory(0)) || self.halted()
    }

    /// Counts `bytes` more of what the checker keeps, checking them against
    /// the memory left at once when they are many, rather than at the next
    /// poll that measures, which may be many statements away. Returns
    /// whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub(super) fn grow(&mut self, bytes: usize) -> bool {
        self.grown += bytes;
        (bytes >= SCRATCH && self.check_memory(0)) || self.halted()
    }
}

#[cfg(test)]
mod tests {
    /// Swaps of state the account counts while it is set aside: the memo,
    /// the frame, its flow and the facts.
    const SWAPS: &[&str] = &[
        "self.memo = ",
        "self.memo.replace(",
        "self.memo.take(",
        "mem::replace(&mut self.memo",
        "mem::take(&mut self.memo",
        "mem::swap(&mut self.memo",
        "self.frame = ",
        "mem::replace(&mut self.frame",
        "mem::take(&mut self.frame",
        "mem::swap(&mut self.frame",
        "self.frame.flow = ",
        "self.facts = ",
        "mem::replace(&mut self.facts",
        "mem::take(&mut self.facts",
        "mem::swap(&mut self.facts",
    ];

    #[test]
    fn only_the_meter_sets_counted_state_aside() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = vec![root.join("typing.rs")];
        for entry in std::fs::read_dir(root.join("typing")).unwrap() {
            sources.push(entry.unwrap().path());
        }
        let mut found = Vec::new();
        for path in sources {
            if path.file_name().is_some_and(|name| name == "meter.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            for (number, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.starts_with("//") {
                    continue;
                }
                if SWAPS.iter().any(|swap| line.contains(swap)) {
                    found.push(format!("{}:{}: {line}", path.display(), number + 1));
                }
            }
        }
        assert!(
            found.is_empty(),
            "set counted state aside through the meter's methods, so the account follows it:\n{}",
            found.join("\n")
        );
    }

    /// The places that go on after a charge or a measure whatever it says,
    /// by file and what they discard, each because nothing is left for a
    /// stop to end there: the budget's stop is the meter's, which the
    /// caller's next check reads.
    const GOING_ON: &[(&str, &str)] = &[
        // A kept site's charge, its last step.
        (
            "construction.rs",
            "let _ = self.meter.charge(visited as u64);",
        ),
        // A host module's name, which the id it returns names.
        ("calls.rs", "let _ = self.grow(name.capacity()"),
        // The last of recording a required file's call, and a read.
        ("check.rs", "let _ = self.grow(grown);"),
        ("check.rs", "let _ = self.grow(bytes);"),
        // A local's names, which its id and its scope need.
        ("check.rs", "let _ = self.grew(3 * name.len());"),
        // A finished walk's last charge, when it is dropped.
        ("walk.rs", "let _ = self.meter.charge(self.visited);"),
        // The type table's own types, added however the check stands.
        ("ty.rs", "let _ = self.transient(heap);"),
        ("ty.rs", "let _ = self.poll();"),
    ];

    #[test]
    fn a_charge_or_measure_is_discarded_only_where_nothing_is_left() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = vec![root.join("typing.rs")];
        for entry in std::fs::read_dir(root.join("typing")).unwrap() {
            sources.push(entry.unwrap().path());
        }
        let mut found = Vec::new();
        for path in sources {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            for (number, line) in text.lines().enumerate() {
                let line = line.trim();
                if !line.starts_with("let _ = ") {
                    continue;
                }
                let allowed = GOING_ON
                    .iter()
                    .any(|(file, prefix)| name == *file && line.starts_with(prefix));
                if !allowed {
                    found.push(format!("{}:{}: {line}", path.display(), number + 1));
                }
            }
        }
        assert!(
            found.is_empty(),
            "stop once a charge or a measure stops the check, or say here why nothing is left:\n{}",
            found.join("\n")
        );
    }

    /// Work that meters nothing, handed on in the checker or the surface
    /// pass, each with why it may be: what matches it, by file.
    const UNMETERED: &[(&str, &str)] = &[
        // Lexing an interpolation again, whose steps the pass charged in
        // advance and whose tokens its estimate counts, asking now and
        // then whether the compilation has stopped.
        (
            "surface/parse.rs",
            "impl crate::compilation::Work for Stopping<'_> {",
        ),
        // A required file's syntax error, reported without its budget.
        (
            "typing/modules.rs",
            "crate::syntax::canonical_syntax(&source, &(), error),",
        ),
        ("typing/modules.rs", "&(),"),
    ];

    /// The functions of the parser, the sources and the loader that the
    /// checker and the surface pass call, each given the caller's work.
    const ENTERED: &[(&str, &str)] = &[
        ("syntax/record.rs", "fn tokens_within("),
        ("syntax/record.rs", "fn parse_with_tokens("),
        ("syntax.rs", "fn canonical_error("),
        ("syntax.rs", "fn canonical_syntax("),
        ("source.rs", "fn parse_error("),
        ("loading.rs", "pub fn source("),
    ];

    /// The lines of the function of `text` whose signature starts with
    /// `signature`, through its closing brace.
    fn body<'t>(text: &'t str, signature: &str) -> Vec<(usize, &'t str)> {
        let lines: Vec<&str> = text.lines().collect();
        let start = lines
            .iter()
            .position(|line| line.contains(signature))
            .unwrap_or_else(|| panic!("no `{signature}`"));
        let mut depth = 0;
        let mut opened = false;
        let mut found = Vec::new();
        for (number, line) in lines.iter().enumerate().skip(start) {
            found.push((number, *line));
            for byte in line.bytes() {
                match byte {
                    b'{' => {
                        depth += 1;
                        opened = true;
                    }
                    b'}' => depth -= 1,
                    _ => (),
                }
            }
            if opened && depth == 0 {
                break;
            }
        }
        found
    }

    #[test]
    fn the_checker_hands_on_only_metered_work() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = Vec::new();
        for module in ["typing", "surface"] {
            sources.push(root.join(format!("{module}.rs")));
            for entry in std::fs::read_dir(root.join(module)).unwrap() {
                let path = entry.unwrap().path();
                if path.file_name().is_some_and(|name| name != "tests.rs") {
                    sources.push(path);
                }
            }
        }
        let relative = |path: &std::path::Path| {
            path.strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        };
        let allowed = |file: &str, line: &str| {
            UNMETERED
                .iter()
                .any(|(allowed, text)| file == *allowed && line.starts_with(text))
        };
        let unmetered = |line: &str| {
            let code = line.split("//").next().unwrap_or("");
            code.contains("&()") || (code.contains("Work for ") && code.starts_with("impl"))
        };
        let mut found = Vec::new();
        for path in sources {
            let file = relative(&path);
            let text = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            for (number, line) in lines.iter().enumerate() {
                let line = line.trim();
                // The unit tests at the end of a file build what they need.
                let next = lines.get(number + 1).map_or("", |next| next.trim());
                if line == "#[cfg(test)]" && next.starts_with("mod ") {
                    break;
                }
                if unmetered(line) && !allowed(&file, line) {
                    found.push(format!("{file}:{}: {line}", number + 1));
                }
            }
        }
        for (file, signature) in ENTERED {
            let text = std::fs::read_to_string(root.join(file)).unwrap();
            for (number, line) in body(&text, signature) {
                let line = line.trim();
                if unmetered(line) && !allowed(file, line) {
                    found.push(format!("{file}:{}: {line}", number + 1));
                }
            }
        }
        assert!(
            found.is_empty(),
            "hand the caller's metered work on, or say here why nothing needs metering:\n{}",
            found.join("\n")
        );
    }
}

#[cfg(test)]
mod budget_tests {
    use crate::{CancellationToken, compilation::Budget};

    /// Checks `source` within `budget`.
    fn checked(source: &str, budget: Budget) -> super::super::Checked {
        checked_as(source, budget, false)
    }

    /// Checks `source` within `budget`, as a required file if `file`.
    fn checked_as(source: &str, budget: Budget, file: bool) -> super::super::Checked {
        checked_with(source, budget, file, None)
    }

    /// Checks `source` within `budget`, as a required file if `file`,
    /// resolving what it requires with `modules`.
    fn checked_with(
        source: &str,
        budget: Budget,
        file: bool,
        modules: Option<&super::super::Modules<'_>>,
    ) -> super::super::Checked {
        let (parsed, tokens, _) = crate::syntax::parse_with_tokens(source, &()).unwrap();
        let declared = crate::declared::Declarations::new();
        super::super::check(&super::super::Input {
            source,
            parsed: &parsed,
            tokens: &tokens,
            hosts: Vec::new(),
            declared: &declared,
            file,
            origin: None,
            modules,
            budget,
            observe: None,
            annotate: false,
        })
    }

    /// Nested `begin`s around assignments of distinct locals, each narrowed
    /// before them: every level's rescue and ensure forget what the levels
    /// inside assign, so checking takes work proportional to the depth
    /// times the locals.
    fn nested_begins(levels: usize, locals: usize) -> String {
        let mut source: String = (0..locals).map(|i| format!("x{i}: int? = 1\n")).collect();
        source.push_str(&"begin\n".repeat(levels));
        source.extend((0..locals).map(|i| format!("x{i} = nil\n")));
        source.push_str(&"rescue\nc = 1\nensure\nc = 2\nend\n".repeat(levels));
        source
    }

    /// Two unions of shapes with optional fields, which a value of one may
    /// fit in any alternative of the other, so relating them in the first
    /// assignment compares every pair.
    fn loose_unions(arms: usize) -> String {
        let union = |prefix: &str, extra: &str| {
            (0..arms)
                .map(|i| format!("{{{prefix}{i}?: int{extra}}}"))
                .collect::<Vec<_>>()
                .join(" | ")
        };
        format!(
            "type A = {}\ntype B = {}\ndef f(x: A, y: B?) -> B?\n  z: A? = x\n  y\nend\ndef g(x: A) -> B\n  x\nend\n",
            union("a", ""),
            union("b", ", x: int")
        )
    }

    #[test]
    fn many_empty_declarations_are_charged_and_polled() {
        // Functions with nothing in them, which the walk for the files a
        // program requires visits even so, before anything else charges
        // them, and before the last function's `require`, which it would
        // then look for.
        use std::sync::atomic::{AtomicBool, Ordering};
        static ASKED: AtomicBool = AtomicBool::new(false);
        let mut source: String = (0..100_000).map(|i| format!("def f{i}\nend\n")).collect();
        source.push_str("def last\n  require(\"far\")\nend\n");
        let resolve = |_: &str, _: Option<&crate::loading::Origin>, _: &mut crate::CallContext| {
            ASKED.store(true, Ordering::Relaxed);
            Err(crate::Error::new(crate::ErrorKind::Name, "no files"))
        };
        let quota = 1_000;
        let stopped = checked_with(
            &source,
            Budget {
                steps: Some(quota),
                ..Budget::default()
            },
            false,
            Some(&resolve),
        );
        assert!(stopped.stopped, "{} steps, not stopped", stopped.steps);
        assert!(stopped.steps < 2 * quota, "{} steps", stopped.steps);
        // The walk stopped at the quota, long before the last function.
        assert!(!ASKED.load(Ordering::Relaxed), "the file was looked for");
    }

    #[test]
    fn a_required_files_check_counts_the_exports_it_copies() {
        // Public functions are exported, copying their names and
        // signatures; private ones are not.
        let long = "e".repeat(1_000);
        let file = |private: &str| -> String {
            (0..200)
                .map(|i| format!("{private}def f{i}{long}(a: int) -> int\n  a\nend\n"))
                .collect()
        };
        let public = checked_as(&file(""), Budget::default(), true);
        let private = checked_as(&file("private "), Budget::default(), true);
        assert!(public.exported.is_some() && private.exported.is_some());
        // Each export holds a copy of its name and one in its signature.
        let copies = 200 * 2 * long.len();
        assert!(
            public.peak_bytes >= private.peak_bytes + copies,
            "{} bytes at the peak with public functions, {} with private ones",
            public.peak_bytes,
            private.peak_bytes
        );
        // A budget the copies would pass stops the check before it makes
        // them, which then exports nothing.
        let stopped = checked_as(
            &file(""),
            Budget {
                memory: Some(public.peak_bytes - copies / 2),
                ..Budget::default()
            },
            true,
        );
        assert!(stopped.stopped && stopped.exported.is_none());
    }

    #[test]
    fn a_required_files_surface_charge_past_the_budget_stops_the_check() {
        // A file of 500 exported functions whose check ends within what
        // the quota leaves it, but not with its surface pass, a step a
        // token, charged: the check stops there, keeping the charge.
        let file: String = (0..500)
            .map(|i| format!("def f{i}(x: int) -> int\n  x\nend\n"))
            .collect();
        let tokens = crate::tooling::tokens(&file).unwrap().len() as u64;
        let alone = checked_as(&file, Budget::default(), true);
        assert!(!alone.stopped);
        let short = |quota| Budget {
            steps: Some(quota),
            ..Budget::default()
        };
        let quota = alone.steps - tokens / 2;
        let stopped = checked_as(&file, short(quota), true);
        assert!(
            stopped.stopped,
            "{} steps for a quota of {quota}",
            stopped.steps
        );
        assert_eq!(stopped.steps, alone.steps);
        // Required, and so charged last but for importing its exports and
        // the few tokens of the check that requires it, it stops that
        // check too, before it imports the 500 exports, a step or more
        // each.
        let mut engine = crate::Engine::new();
        engine
            .set_module_sources(std::collections::BTreeMap::from([(
                "b.vibe".to_owned(),
                file,
            )]))
            .unwrap();
        let resolve = |path: &str,
                       origin: Option<&crate::loading::Origin>,
                       context: &mut crate::CallContext| {
            engine.loader.source(path, origin, context)
        };
        let source = "require(\"b\")\n";
        let full = checked_with(source, Budget::default(), false, Some(&resolve));
        assert!(!full.stopped);
        let quota = full.steps - tokens / 2;
        let stopped = checked_with(source, short(quota), false, Some(&resolve));
        assert!(
            stopped.stopped && stopped.steps + 500 <= full.steps,
            "{} steps for a quota of {quota}, of {}",
            stopped.steps,
            full.steps
        );
    }

    #[test]
    fn interpolations_parse_again_within_the_step_quota() {
        // Fifty strings, each interpolating a 2,000-element literal, which
        // the spans parse again before the check starts.
        let literal = vec!["1"; 2_000].join(", ");
        let source: String = (0..50)
            .map(|i| format!("x{i} = \"#{{[{literal}].length}}\"\n"))
            .collect();
        let full = checked(&source, Budget::default());
        assert!(!full.stopped);
        let quota = 1_000;
        let stopped = checked(
            &source,
            Budget {
                steps: Some(quota),
                ..Budget::default()
            },
        );
        assert!(stopped.stopped);
        assert!(
            stopped.steps < 2 * quota,
            "{} steps for a quota of {quota}, of {}",
            stopped.steps,
            full.steps
        );
    }

    #[test]
    fn the_check_stops_within_its_budget() {
        for source in [nested_begins(60, 300), loose_unions(200)] {
            let full = checked(&source, Budget::default());
            assert!(!full.stopped);
            // A step quota stops the check within a statement's or a type
            // operation's work of it, long before the check would end.
            let quota = full.steps / 10;
            let stopped = checked(
                &source,
                Budget {
                    steps: Some(quota),
                    ..Budget::default()
                },
            );
            assert!(stopped.stopped);
            assert!(
                stopped.steps < 2 * quota,
                "{} steps for a quota of {quota}",
                stopped.steps
            );
            // A deadline that has passed, or a cancellation, stops it at
            // its first poll.
            let cancellation = CancellationToken::new();
            cancellation.cancel();
            for budget in [
                Budget {
                    deadline: Some(std::time::Instant::now()),
                    ..Budget::default()
                },
                Budget {
                    cancellation: Some(cancellation),
                    ..Budget::default()
                },
            ] {
                let stopped = checked(&source, budget);
                assert!(stopped.stopped);
                assert!(
                    stopped.steps < full.steps / 100,
                    "{} steps of {}",
                    stopped.steps,
                    full.steps
                );
            }
        }
    }
}
