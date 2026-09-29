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

/// The running totals, which the checker and its type table share.
#[derive(Debug, Default)]
pub(crate) struct Meter {
    budget: Budget,
    steps: AtomicU64,
    /// The bytes the checker's tables other than the type table held when
    /// it last polled, which the type table adds its own to.
    outside: AtomicUsize,
    peak: AtomicUsize,
    stopped: AtomicBool,
    polls: AtomicU64,
}

impl Meter {
    pub fn new(budget: Budget) -> Arc<Self> {
        Arc::new(Self {
            budget,
            ..Self::default()
        })
    }

    /// Adds `steps` of work to the total.
    pub fn charge(&self, steps: u64) {
        self.steps.fetch_add(steps, Relaxed);
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
        self.stopped.store(true, Relaxed);
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
        let held = types + self.outside.load(Relaxed);
        self.reach(held);
        held
    }

    /// Records that the check held `bytes` at some point, as while a
    /// required file was checked.
    pub fn reach(&self, bytes: usize) {
        self.peak.fetch_max(bytes, Relaxed);
    }

    /// Records the type table's `bytes` with `extra` an operation holds
    /// while it runs, stopping the check if they pass the memory left.
    pub fn transient(&self, types: usize, extra: usize) {
        let held = self.held(types) + extra;
        self.reach(held);
        if self.budget.memory.is_some_and(|left| held > left) {
            self.stop();
        }
    }

    /// Checks the totals against the budget: the steps on every poll, and
    /// on every 64th the memory `measure` reports, the deadline and the
    /// cancellation token, whose checks cost more. Once past any of them
    /// the check stays stopped.
    pub fn poll(&self, measure: impl FnOnce() -> usize) -> bool {
        if self.stopped() {
            return true;
        }
        if self.budget.steps.is_some_and(|left| self.steps() > left) {
            self.stop();
            return true;
        }
        if self.polls.fetch_add(1, Relaxed) % 64 == 0 {
            let held = measure();
            if self.budget.interrupted() || self.budget.memory.is_some_and(|left| held > left) {
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
            TokenKind::String(bytes) => bytes.capacity(),
            TokenKind::Template(parts) => parts.capacity() * size_of::<std::ops::Range<usize>>(),
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
    pub(super) fn over_budget(&mut self) -> bool {
        if !self.stopped {
            self.stopped = self.meter.poll(|| self.held());
        }
        self.stopped
    }

    /// The memory the checker holds: its tables other than the type table,
    /// which the meter keeps for the type table's own polls, and the type
    /// table.
    pub(super) fn held(&self) -> usize {
        self.meter.outside(self.outside());
        self.meter.held(self.types.bytes())
    }

    /// Measures the declarations once they change, which is rarely: the
    /// program's functions, classes, modules and enums, the host's, and
    /// their names in the type table.
    pub(super) fn declared(&mut self) {
        self.declared_bytes = self.program.heap() + self.types.names.heap() + self.modules.heap();
    }

    /// What the checker's tables other than the type table hold.
    fn outside(&self) -> usize {
        let frame = &self.frame;
        let contexts: usize = frame.contexts.iter().map(Heap::heap).sum();
        vec(&self.diagnostics)
            + vec(&self.calls)
            + map(&self.constants)
            + self.grown
            + self.declared_bytes
            + self.converter.grown()
            + self.spans.bytes()
            + frame.heap()
            + contexts
            + self.saved
            + self.purposes.capacity() * size_of::<super::check::Purpose>()
            + self.memo.as_ref().map_or(0, super::Memo::bytes)
            + set(&self.write_chain)
            + map(&self.fetch_receivers)
            + self.facts.bytes()
            + self.construction.bytes()
            + self.program.grown()
            + self.assigns.bytes()
    }

    /// Records `bytes` of scratch that a walk over the syntax held beside
    /// the tables, for the budget and the peak.
    pub(super) fn transient(&self, bytes: usize) {
        self.meter.transient(self.types.bytes(), bytes);
    }

    /// Counts a diagnostic the checker keeps.
    pub(super) fn keep(&mut self, diagnostic: &crate::diagnostic::Diagnostic) {
        self.grown += diagnostic.heap();
    }
}
