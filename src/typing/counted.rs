//! The checker's tables, which count their growth before it happens.
//!
//! A table the checker keeps is measured with the rest at each of the
//! checker's measures. Between measures it may grow, and a table that grows
//! by a source-sized amount could otherwise pass the memory left before the
//! next measure saw it. Each table here, before it takes more storage, asks
//! the meter whether the check can hold its old and new storage at once,
//! beside what the check held when last measured and what its tables grew
//! since, and records what it grew by, which the next measure of its side
//! takes back into its own count. What an element owns on the heap, such
//! as a copy of a name, is counted with it as the table takes it, or before
//! it is made, when its caller counts it first. A table the budget refuses
//! to grow keeps what it had and stores nothing, and its caller stops.

use super::meter::{Heap, Meter, Side, btree_storage, map, set, table};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    hash::Hash,
    mem::size_of,
    ops::Deref,
    sync::Arc,
};

/// What a table the budget refused to grow gives back: nothing changed,
/// and the check has stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Refused;

/// The meter a table's growth is charged to, and the measure that counts
/// the table.
#[derive(Clone, Copy)]
pub(crate) struct Ledger<'m> {
    meter: &'m Meter,
    side: Side,
    /// Whether it records growth without refusing it.
    regardless: bool,
}

impl<'m> Ledger<'m> {
    pub fn new(meter: &'m Meter, side: Side) -> Self {
        Self {
            meter,
            side,
            regardless: false,
        }
    }

    /// This ledger, recording growth without refusing it, for a table
    /// whose entries have fixed places it must keep however the budget
    /// stands, such as the type table's own types. The next measure of
    /// its side reads a stop.
    pub fn regardless(self) -> Self {
        Self {
            regardless: true,
            ..self
        }
    }

    /// Admits a table's growth, which holds `moment` bytes at once while
    /// it grows: returns what the check would hold then, for
    /// [`Self::grew`]. Refused when the check could not hold them beside
    /// what it holds, which stops it.
    fn admit(self, moment: usize) -> Result<usize, Refused> {
        if self.regardless {
            return Ok(0);
        }
        self.meter.admit(moment).ok_or(Refused)
    }

    /// Refused once the check has stopped, as an admission is, for a
    /// change that takes no more room: a table keeps nothing more after
    /// the stop, so a loop adding to it stops too.
    fn open(self) -> Result<(), Refused> {
        if self.regardless || !self.meter.stopped() {
            Ok(())
        } else {
            Err(Refused)
        }
    }

    /// Records what a table grew by once it has, in a growth
    /// [`Self::admit`] found would hold `peak` bytes at once.
    fn grew(self, bytes: usize, peak: usize) {
        self.meter.settle(self.side, bytes, peak);
    }

    /// Records `bytes` a table kept however the budget stands, such as
    /// the nodes undoing a change copies, which must be undone.
    pub fn kept(self, bytes: usize) {
        self.grew(bytes, 0);
    }

    /// Counts `bytes` a table will keep beside its own storage, such as a
    /// name it copies, before they are made; what it counted, for the
    /// `_kept` methods of the tables that take them.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn keep(self, bytes: usize) -> Result<Kept, Refused> {
        if bytes == 0 {
            self.open()?;
            return Ok(Kept(0));
        }
        let peak = self.admit(bytes)?;
        self.grew(bytes, peak);
        Ok(Kept(bytes))
    }
}

impl Meter {
    /// The ledger of the checker's tables other than the type table.
    pub fn tables(&self) -> Ledger<'_> {
        Ledger::new(self, Side::Tables)
    }

    /// The ledger of the type table's own tables.
    pub fn types(&self) -> Ledger<'_> {
        Ledger::new(self, Side::Types)
    }

    /// The ledger of the declarations.
    pub fn declarations(&self) -> Ledger<'_> {
        Ledger::new(self, Side::Declarations)
    }

    /// The ledger of the lists an operation builds and drops.
    pub fn scratch_lists(&self) -> Ledger<'_> {
        Ledger::new(self, Side::Scratch)
    }
}

/// What an element of a counted table owns on the heap beyond its slot:
/// a copy of a name, a list. A table counts it when it takes the element,
/// with the slot, and the measure of the table's side counts it after. A
/// value an `Rc` or an `Arc` shares owns nothing here, and neither does a
/// counted table, whose storage is counted as it grows: whoever makes
/// them counts them.
pub(crate) trait Owned {
    fn owned(&self) -> usize;
}

/// Types that own nothing on the heap.
macro_rules! nothing {
    ($($t:ty),* $(,)?) => {
        $(impl Owned for $t {
            fn owned(&self) -> usize {
                0
            }
        })*
    };
}

nothing!(
    bool,
    u8,
    u16,
    u32,
    u64,
    usize,
    i64,
    super::ty::Ty,
    super::flow::VarState,
    crate::members::direct::Base,
);

impl Owned for crate::diagnostic::Diagnostic {
    fn owned(&self) -> usize {
        self.heap()
    }
}

impl Owned for super::ReceiverType {
    fn owned(&self) -> usize {
        self.heap()
    }
}

/// Its name, which the measure of the files a check loads counts.
impl Owned for crate::loading::Origin {
    fn owned(&self) -> usize {
        self.name().len()
    }
}

impl<T: ?Sized> Owned for &T {
    fn owned(&self) -> usize {
        0
    }
}

/// A cursor into a list another owns, such as what is left of a level of
/// a walk.
impl<T> Owned for std::slice::Iter<'_, T> {
    fn owned(&self) -> usize {
        0
    }
}

impl<T: ?Sized> Owned for std::rc::Rc<T> {
    fn owned(&self) -> usize {
        0
    }
}

impl<T: ?Sized> Owned for Arc<T> {
    fn owned(&self) -> usize {
        0
    }
}

impl Owned for String {
    fn owned(&self) -> usize {
        self.capacity()
    }
}

impl Owned for Box<str> {
    fn owned(&self) -> usize {
        self.len()
    }
}

impl<T: Owned> Owned for Option<T> {
    fn owned(&self) -> usize {
        self.as_ref().map_or(0, Owned::owned)
    }
}

impl<T: Owned, E: Owned> Owned for Result<T, E> {
    fn owned(&self) -> usize {
        match self {
            Ok(value) => value.owned(),
            Err(error) => error.owned(),
        }
    }
}

impl<A: Owned, B: Owned> Owned for (A, B) {
    fn owned(&self) -> usize {
        self.0.owned() + self.1.owned()
    }
}

impl<A: Owned, B: Owned, C: Owned> Owned for (A, B, C) {
    fn owned(&self) -> usize {
        self.0.owned() + self.1.owned() + self.2.owned()
    }
}

impl<T: Owned> Owned for Vec<T> {
    fn owned(&self) -> usize {
        self.capacity() * size_of::<T>() + self.iter().map(Owned::owned).sum::<usize>()
    }
}

impl<T> Owned for CountedVec<T> {
    fn owned(&self) -> usize {
        0
    }
}

impl<K, V> Owned for CountedMap<K, V> {
    fn owned(&self) -> usize {
        0
    }
}

impl<T> Owned for CountedSet<T> {
    fn owned(&self) -> usize {
        0
    }
}

impl<T> Owned for CountedBTreeSet<T> {
    fn owned(&self) -> usize {
        0
    }
}

/// A scratch list counts itself.
impl<T> Owned for ScratchVec<T> {
    fn owned(&self) -> usize {
        0
    }
}

/// What [`Ledger::keep`] counted for the elements a caller copies before
/// it stores them, which the `_kept` methods of the tables take from as
/// they store them.
#[derive(Debug)]
pub(crate) struct Kept(usize);

impl Kept {
    /// Takes `bytes` an element owns from what was counted for it.
    fn spend(&mut self, bytes: usize) {
        debug_assert!(
            bytes <= self.0,
            "an element owns {bytes} bytes, more than the {} counted for it",
            self.0
        );
        self.0 = self.0.saturating_sub(bytes);
    }
}

/// The steps of sorting `length` elements: a step for each 64 of the
/// comparisons a sort may make, about the length times its logarithm, as
/// the type table charges its work.
fn sort_steps(length: usize) -> u64 {
    let comparisons = length.saturating_mul((usize::BITS - length.leading_zeros()) as usize);
    (comparisons / 64) as u64
}

/// Charges sorting `length` elements, and asks the deadline and the
/// cancellation once the steps are many, and admits the `scratch` a
/// stable sort takes beside the list for a moment; refused when that stops
/// the check.
fn pace_sort(meter: &Meter, length: usize, scratch: usize) -> Result<(), Refused> {
    let steps = sort_steps(length);
    if steps > 0 && (meter.charge(steps) || meter.budget().interrupted()) {
        meter.stop();
        return Err(Refused);
    }
    if scratch > 0 && meter.admit(scratch).is_none() {
        return Err(Refused);
    }
    Ok(())
}

/// Sorts `list` as `sort_unstable_by` does, once its steps are charged and
/// the deadline and the cancellation asked; refused, leaving the list as
/// it was, when that stops the check.
#[must_use = "a refusal stops the check, whose list is then left unsorted"]
pub(crate) fn sort_unstable_by<T>(
    meter: &Meter,
    list: &mut [T],
    compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
) -> Result<(), Refused> {
    pace_sort(meter, list.len(), 0)?;
    list.sort_unstable_by(compare);
    Ok(())
}

/// Sorts `list` as `sort_by` does, keeping equal elements in order, once
/// its steps and the scratch a stable sort takes, as long as the list, are
/// counted; refused, leaving the list as it was, when that stops the check.
#[must_use = "a refusal stops the check, whose list is then left unsorted"]
pub(crate) fn sort_by<T>(
    meter: &Meter,
    list: &mut [T],
    compare: impl FnMut(&T, &T) -> std::cmp::Ordering,
) -> Result<(), Refused> {
    pace_sort(meter, list.len(), std::mem::size_of_val(list))?;
    list.sort_by(compare);
    Ok(())
}

/// Text written through the meter, such as a diagnostic's message: each
/// piece is counted before the string grows to take it, with its old and
/// new storage while it grows, and while it is written, as a scratch
/// list is. Once the budget refuses a piece, it writes nothing more, and
/// the check has stopped.
pub(crate) struct Text<'m> {
    text: String,
    ledger: Ledger<'m>,
    refused: bool,
}

impl<'m> Text<'m> {
    pub fn new(meter: &'m Meter) -> Self {
        Self {
            text: String::new(),
            ledger: meter.scratch_lists(),
            refused: false,
        }
    }

    /// Writes `piece`, once it is counted.
    pub fn push_str(&mut self, piece: &str) {
        if self.refused {
            return;
        }
        let (length, capacity) = (self.text.len(), self.text.capacity());
        let needed = length.saturating_add(piece.len());
        if needed > capacity {
            let target = needed.max(capacity.saturating_mul(2)).max(16);
            // The old text, counted already, stays while the new storage
            // is taken.
            if self.ledger.admit(target).is_err() {
                self.refused = true;
                return;
            }
            self.text.reserve_exact(target - length);
            // A text is scratch of a moment, recorded but not reported: the
            // account's observer reads the check at its measures and its
            // tables' growth.
            self.ledger
                .meter
                .record(self.ledger.side, self.text.capacity() - capacity);
        }
        self.text.push_str(piece);
    }

    pub fn push(&mut self, c: char) {
        self.push_str(c.encode_utf8(&mut [0; 4]));
    }

    /// Writes `args`, as `write!` writes them; a refusal stops the writing.
    pub fn write(&mut self, args: std::fmt::Arguments<'_>) {
        // A refusal is recorded, and writes nothing more.
        if std::fmt::Write::write_fmt(self, args).is_err() {
            self.refused = true;
        }
    }

    /// The text, no longer counted here: whoever keeps it counts it. A
    /// text the budget refused is empty.
    pub fn finish(mut self) -> String {
        self.ledger.meter.dropped(self.text.capacity());
        if self.refused {
            return String::new();
        }
        std::mem::take(&mut self.text)
    }
}

impl std::fmt::Write for Text<'_> {
    fn write_str(&mut self, piece: &str) -> std::fmt::Result {
        self.push_str(piece);
        if self.refused {
            return Err(std::fmt::Error);
        }
        Ok(())
    }
}

/// `args` written out through the meter, as [`Text`] writes; empty once the
/// budget refuses it, which stops the check.
pub(crate) fn text(meter: &Meter, args: std::fmt::Arguments<'_>) -> String {
    let mut text = Text::new(meter);
    // A refusal leaves the text empty, and the check stopped.
    if std::fmt::write(&mut text, args).is_err() {
        text.refused = true;
    }
    text.finish()
}

/// A list an operation builds and drops, whose storage and elements are
/// counted before it takes them, as a table's are, and while it lives,
/// since no measure counts it: they are taken back when it is dropped.
pub(crate) struct ScratchVec<T> {
    list: CountedVec<T>,
    /// What its elements own.
    owned: usize,
    meter: Arc<Meter>,
}

impl<T: Owned> ScratchVec<T> {
    pub fn new(meter: &Arc<Meter>) -> Self {
        Self {
            list: CountedVec::new(),
            owned: 0,
            meter: Arc::clone(meter),
        }
    }

    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn reserve(&mut self, additional: usize) -> Result<(), Refused> {
        self.list.reserve(self.meter.scratch_lists(), additional)
    }

    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn push(&mut self, value: T) -> Result<(), Refused> {
        let owned = value.owned();
        self.list.push(self.meter.scratch_lists(), value)?;
        self.owned += owned;
        Ok(())
    }

    /// Makes room for `additional` more elements, and counts the `owned`
    /// bytes they will own, before any is made, for
    /// [`Self::push_within`] to take.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn reserve_with(&mut self, additional: usize, owned: usize) -> Result<(), Refused> {
        self.list.reserve(self.meter.scratch_lists(), additional)?;
        self.meter.scratch_lists().keep(owned)?;
        self.owned += owned;
        Ok(())
    }

    /// Adds `value` in room [`Self::reserve_with`] made for it, which
    /// counted what it owns.
    pub fn push_within(&mut self, value: T) {
        debug_assert!(self.list.len() < self.list.capacity(), "room is made first");
        self.list.push_moved(value);
    }

    /// Adds `value`, or, when the budget refuses it room, drops it: the
    /// check has stopped, and its caller unwinds without reading the list.
    pub fn add(&mut self, value: T) {
        // A refusal stops the check, which the caller's next check reads.
        if self.push(value).is_err() {
            debug_assert!(self.meter.stopped());
        }
    }

    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn extend_from_slice(&mut self, values: &[T]) -> Result<(), Refused>
    where
        T: Clone,
    {
        let owned = values.iter().map(Owned::owned).sum::<usize>();
        self.list
            .extend_from_slice(self.meter.scratch_lists(), values)?;
        self.owned += owned;
        Ok(())
    }

    pub fn dedup(&mut self)
    where
        T: PartialEq,
    {
        self.list.dedup();
    }

    /// Takes the last element, giving back what it owns.
    pub fn pop(&mut self) -> Option<T> {
        let value = self.list.pop()?;
        let owned = value.owned();
        self.meter.dropped(owned);
        self.owned -= owned;
        Some(value)
    }

    /// The list, no longer counted here: whoever keeps it counts it.
    pub fn into_vec(mut self) -> Vec<T> {
        self.give_back();
        std::mem::take(&mut self.list).into_vec()
    }
}

impl<T> ScratchVec<T> {
    /// Takes back what the list and its elements hold.
    fn give_back(&mut self) {
        self.meter
            .dropped(self.list.capacity() * size_of::<T>() + self.owned);
        self.owned = 0;
    }
}

impl<T> Deref for ScratchVec<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.list
    }
}

impl<T> std::ops::DerefMut for ScratchVec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.list
    }
}

impl<T> Drop for ScratchVec<T> {
    fn drop(&mut self) {
        self.give_back();
    }
}

/// A set an operation builds and drops, counted as [`ScratchVec`] is.
pub(crate) struct ScratchSet<T> {
    set: CountedSet<T>,
    /// What its elements own.
    owned: usize,
    meter: Arc<Meter>,
}

impl<T: Eq + Hash + Owned> ScratchSet<T> {
    pub fn new(meter: &Arc<Meter>) -> Self {
        Self {
            set: CountedSet::new(),
            owned: 0,
            meter: Arc::clone(meter),
        }
    }

    /// Adds `value`; whether it is new.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert(&mut self, value: T) -> Result<bool, Refused> {
        let owned = value.owned();
        let added = self.set.insert(self.meter.scratch_lists(), value)?;
        self.owned += owned;
        Ok(added)
    }
}

impl<T> Deref for ScratchSet<T> {
    type Target = HashSet<T>;

    fn deref(&self) -> &HashSet<T> {
        &self.set
    }
}

/// A map an operation builds and drops, counted as a [`ScratchSet`] is:
/// its table and what its entries own, while it lives.
pub(crate) struct ScratchMap<K, V> {
    map: CountedMap<K, V>,
    /// What its keys and values own.
    owned: usize,
    meter: Arc<Meter>,
}

impl<K: Eq + Hash + Owned, V: Owned> ScratchMap<K, V> {
    pub fn new(meter: &Arc<Meter>) -> Self {
        Self {
            map: CountedMap::new(),
            owned: 0,
            meter: Arc::clone(meter),
        }
    }

    /// Stores `value` under `key`, in place of any value the key had.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert(&mut self, key: K, value: V) -> Result<(), Refused> {
        // A key the map has keeps its entry, and what the value it gives
        // way to owned is given back.
        if let Some(slot) = self.map.0.get_mut(&key) {
            let owned = value.owned();
            self.meter.scratch_lists().keep(owned)?;
            let replaced = std::mem::replace(slot, value).owned();
            self.meter.dropped(replaced);
            self.owned = self.owned - replaced + owned;
            return Ok(());
        }
        let owned = key.owned() + value.owned();
        self.map.insert(self.meter.scratch_lists(), key, value)?;
        self.owned += owned;
        Ok(())
    }
}

impl<K, V> Deref for ScratchMap<K, V> {
    type Target = HashMap<K, V>;

    fn deref(&self) -> &HashMap<K, V> {
        &self.map
    }
}

impl<K, V> Drop for ScratchMap<K, V> {
    fn drop(&mut self) {
        self.meter.dropped(map(&self.map.0) + self.owned);
    }
}

impl<T> Drop for ScratchSet<T> {
    fn drop(&mut self) {
        self.meter.dropped(set(&self.set.0) + self.owned);
    }
}

/// A list whose growth, and what its elements own, is counted before it
/// happens. It reads as a slice; it grows only through the methods that
/// take a [`Ledger`] or what one kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CountedVec<T>(Vec<T>);

impl<T> Default for CountedVec<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> CountedVec<T> {
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }

    /// The list, for a measure of it.
    pub fn as_vec(&self) -> &Vec<T> {
        &self.0
    }

    pub fn into_vec(self) -> Vec<T> {
        self.0
    }

    /// Makes room for `additional` more elements. A list that must grow
    /// takes at least twice its storage, counted with its old storage
    /// first, and exactly that much.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn reserve(&mut self, ledger: Ledger<'_>, additional: usize) -> Result<(), Refused> {
        self.room(ledger, additional, 0)
    }

    /// Makes room for `additional` more elements that own `owned` bytes,
    /// counted together.
    fn room(&mut self, ledger: Ledger<'_>, additional: usize, owned: usize) -> Result<(), Refused> {
        let capacity = self.0.capacity();
        let needed = self.0.len().saturating_add(additional);
        if needed <= capacity {
            return ledger.keep(owned).map(drop);
        }
        let target = needed.max(capacity.saturating_mul(2)).max(4);
        let size = size_of::<T>();
        // The old storage, which the check holds already, stays while the
        // new is taken, so the new is all the moment adds.
        let moment = target.saturating_mul(size).saturating_add(owned);
        let peak = ledger.admit(moment)?;
        self.0.reserve_exact(target - self.0.len());
        // The allocator is asked for exactly this much; anything more it
        // gives is counted too.
        ledger.grew((self.0.capacity() - capacity) * size + owned, peak);
        Ok(())
    }

    /// Adds `value`, with what it owns, counted first; a value the budget
    /// refuses is dropped.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn push(&mut self, ledger: Ledger<'_>, value: T) -> Result<(), Refused>
    where
        T: Owned,
    {
        self.room(ledger, 1, value.owned())?;
        self.0.push(value);
        Ok(())
    }

    /// Adds `value` however the budget stands, counting what the list
    /// grows by and what the value owns, for a list whose entries have
    /// fixed places that must be kept; the next measure reads a stop.
    pub fn push_regardless(&mut self, ledger: Ledger<'_>, value: T)
    where
        T: Owned,
    {
        // A ledger that records regardless refuses nothing.
        match self.room(ledger.regardless(), 1, value.owned()) {
            Ok(()) | Err(Refused) => self.0.push(value),
        }
    }

    /// Adds `value`, which owns nothing, in room [`Self::reserve`] made
    /// for it, so that a caller that changes several tables can count them
    /// all before it changes any.
    pub fn push_within(&mut self, value: T)
    where
        T: Owned,
    {
        debug_assert_eq!(value.owned(), 0, "what an element owns is counted first");
        debug_assert!(self.0.len() < self.0.capacity(), "room is made first");
        self.0.push(value);
    }

    /// Adds `value` in room [`Self::reserve`] made for it, taking what it
    /// owns from what `kept` counted for it.
    pub fn push_kept(&mut self, kept: &mut Kept, value: T)
    where
        T: Owned,
    {
        kept.spend(value.owned());
        debug_assert!(self.0.len() < self.0.capacity(), "room is made first");
        self.0.push(value);
    }

    /// Adds the value `make` copies, room for it and the `bytes` it owns
    /// counted before it is made; a value the budget refuses is not made.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn push_made(
        &mut self,
        ledger: Ledger<'_>,
        bytes: usize,
        make: impl FnOnce() -> T,
    ) -> Result<(), Refused>
    where
        T: Owned,
    {
        self.reserve(ledger, 1)?;
        let mut kept = ledger.keep(bytes)?;
        self.push_kept(&mut kept, make());
        Ok(())
    }

    /// Adds `value` in room [`Self::reserve`] made for it, whose place it
    /// moved from counted what it owns until now, such as the state a loop
    /// was left in, taken from the loop once it ends.
    pub fn push_moved(&mut self, value: T) {
        debug_assert!(self.0.len() < self.0.capacity(), "room is made first");
        self.0.push(value);
    }

    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn extend_from_slice(&mut self, ledger: Ledger<'_>, values: &[T]) -> Result<(), Refused>
    where
        T: Clone + Owned,
    {
        let owned = values.iter().map(Owned::owned).sum::<usize>();
        self.room(ledger, values.len(), owned)?;
        self.0.extend_from_slice(values);
        Ok(())
    }

    pub fn pop(&mut self) -> Option<T> {
        self.0.pop()
    }

    pub fn truncate(&mut self, length: usize) {
        self.0.truncate(length);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn dedup(&mut self)
    where
        T: PartialEq,
    {
        self.0.dedup();
    }
}

impl<T> Deref for CountedVec<T> {
    type Target = [T];

    fn deref(&self) -> &[T] {
        &self.0
    }
}

/// Its elements change in place through the slice, which cannot grow.
impl<T> std::ops::DerefMut for CountedVec<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.0
    }
}

impl<T> IntoIterator for CountedVec<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, T> IntoIterator for &'a CountedVec<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T: Heap> Heap for CountedVec<T> {
    fn heap(&self) -> usize {
        self.0.heap()
    }
}

/// A hash map whose growth is counted before it happens. It reads as a
/// map; it grows only through the methods that take a [`Ledger`].
#[derive(Clone, Debug)]
pub(crate) struct CountedMap<K, V>(HashMap<K, V>);

impl<K, V> Default for CountedMap<K, V> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<K: Eq + Hash, V> CountedMap<K, V> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes room for `additional` more entries. A table that must grow
    /// is counted, with its old table, at the size it grows to: room for
    /// the entries it needs, or for one more than it has room for, which
    /// its buckets round up to twice as many.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn reserve(&mut self, ledger: Ledger<'_>, additional: usize) -> Result<(), Refused> {
        self.room(ledger, additional, 0)
    }

    /// Makes room for `additional` more entries that own `owned` bytes,
    /// counted together.
    fn room(&mut self, ledger: Ledger<'_>, additional: usize, owned: usize) -> Result<(), Refused> {
        let capacity = self.0.capacity();
        let needed = self.0.len().saturating_add(additional);
        if needed <= capacity {
            return ledger.keep(owned).map(drop);
        }
        let before = map(&self.0);
        // The old table, which the check holds already, stays while the
        // new is taken, so the new is all the moment adds.
        let most = table::<(K, V)>(needed.max(capacity.saturating_add(1)));
        let peak = ledger.admit(most.saturating_add(owned))?;
        self.0.reserve(additional);
        ledger.grew(map(&self.0).saturating_sub(before) + owned, peak);
        Ok(())
    }

    /// Stores `value` under `key`, with what they own, counted first,
    /// giving back the value it replaces. A key and value the budget
    /// refuses are dropped.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert(&mut self, ledger: Ledger<'_>, key: K, value: V) -> Result<Option<V>, Refused>
    where
        K: Owned,
        V: Owned,
    {
        ledger.open()?;
        // A table with room takes it, new or not, without growing, and a
        // key it has already is counted as if it were new.
        if self.0.len() >= self.0.capacity() {
            if let Some(slot) = self.0.get_mut(&key) {
                ledger.keep(value.owned())?;
                return Ok(Some(std::mem::replace(slot, value)));
            }
        }
        self.room(ledger, 1, key.owned() + value.owned())?;
        Ok(self.0.insert(key, value))
    }

    /// Stores `value` under `key`, which own nothing, in room
    /// [`Self::reserve`] made for it, giving back the value it replaces.
    pub fn insert_within(&mut self, key: K, value: V) -> Option<V>
    where
        K: Owned,
        V: Owned,
    {
        debug_assert_eq!(
            key.owned() + value.owned(),
            0,
            "what an entry owns is counted first"
        );
        debug_assert!(
            self.0.len() < self.0.capacity() || self.0.contains_key(&key),
            "room is made first"
        );
        self.0.insert(key, value)
    }

    /// Stores `value` under `key` in room [`Self::reserve`] made for it,
    /// taking what they own from what `kept` counted for them, and giving
    /// back the value it replaces.
    pub fn insert_kept(&mut self, kept: &mut Kept, key: K, value: V) -> Option<V>
    where
        K: Owned,
        V: Owned,
    {
        kept.spend(key.owned() + value.owned());
        debug_assert!(
            self.0.len() < self.0.capacity() || self.0.contains_key(&key),
            "room is made first"
        );
        self.0.insert(key, value)
    }

    /// Stores `value` under the key `make` copies, which it does not have:
    /// room for the entry, and the `bytes` the copy owns with what `value`
    /// owns, are counted before the copy is made. A key the budget refuses
    /// is not made, and `value` is dropped.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert_made(
        &mut self,
        ledger: Ledger<'_>,
        bytes: usize,
        make: impl FnOnce() -> K,
        value: V,
    ) -> Result<(), Refused>
    where
        K: Owned,
        V: Owned,
    {
        self.reserve(ledger, 1)?;
        let mut kept = ledger.keep(bytes.saturating_add(value.owned()))?;
        self.insert_kept(&mut kept, make(), value);
        Ok(())
    }

    /// Stores `value` under `key` however the budget stands, counting what
    /// the map grows by and what they own, for a map whose entries must
    /// all be kept; the next measure reads a stop.
    pub fn insert_regardless(&mut self, ledger: Ledger<'_>, key: K, value: V)
    where
        K: Owned,
        V: Owned,
    {
        // A ledger that records regardless refuses nothing.
        match self.room(ledger.regardless(), 1, key.owned() + value.owned()) {
            Ok(()) | Err(Refused) => {
                self.0.insert(key, value);
            }
        }
    }

    /// The value under `key`, made by `make` first, and counted with the
    /// key and what they own, if the map has none.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn get_or_insert_with(
        &mut self,
        ledger: Ledger<'_>,
        key: K,
        make: impl FnOnce() -> V,
    ) -> Result<&mut V, Refused>
    where
        K: Owned,
        V: Owned,
    {
        if self.0.len() >= self.0.capacity() && !self.0.contains_key(&key) {
            self.reserve(ledger, 1)?;
        }
        match self.0.entry(key) {
            std::collections::hash_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
            std::collections::hash_map::Entry::Vacant(entry) => {
                let value = make();
                ledger.keep(entry.key().owned() + value.owned())?;
                Ok(entry.insert(value))
            }
        }
    }

    pub fn get_mut<Q>(&mut self, key: &Q) -> Option<&mut V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.0.get_mut(key)
    }

    pub fn remove<Q>(&mut self, key: &Q) -> Option<V>
    where
        K: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.0.remove(key)
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn values_mut(&mut self) -> std::collections::hash_map::ValuesMut<'_, K, V> {
        self.0.values_mut()
    }

    pub fn into_map(self) -> HashMap<K, V> {
        self.0
    }
}

impl<K, V> Deref for CountedMap<K, V> {
    type Target = HashMap<K, V>;

    fn deref(&self) -> &HashMap<K, V> {
        &self.0
    }
}

impl<'a, K, V> IntoIterator for &'a CountedMap<K, V> {
    type Item = (&'a K, &'a V);
    type IntoIter = std::collections::hash_map::Iter<'a, K, V>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<K: Heap, V: Heap> Heap for CountedMap<K, V> {
    fn heap(&self) -> usize {
        self.0.heap()
    }
}

/// A hash set whose growth is counted before it happens. It reads as a
/// set; it grows only through the methods that take a [`Ledger`].
#[derive(Clone, Debug)]
pub(crate) struct CountedSet<T>(HashSet<T>);

impl<T> Default for CountedSet<T> {
    fn default() -> Self {
        Self(HashSet::new())
    }
}

impl<T: Eq + Hash> CountedSet<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Makes room for `additional` more elements, counted as a
    /// [`CountedMap`] counts its growth.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn reserve(&mut self, ledger: Ledger<'_>, additional: usize) -> Result<(), Refused> {
        self.room(ledger, additional, 0)
    }

    /// Makes room for `additional` more elements that own `owned` bytes,
    /// counted together.
    fn room(&mut self, ledger: Ledger<'_>, additional: usize, owned: usize) -> Result<(), Refused> {
        let capacity = self.0.capacity();
        let needed = self.0.len().saturating_add(additional);
        if needed <= capacity {
            return ledger.keep(owned).map(drop);
        }
        let before = set(&self.0);
        // The old table, which the check holds already, stays while the
        // new is taken, so the new is all the moment adds.
        let most = table::<T>(needed.max(capacity.saturating_add(1)));
        let peak = ledger.admit(most.saturating_add(owned))?;
        self.0.reserve(additional);
        ledger.grew(set(&self.0).saturating_sub(before) + owned, peak);
        Ok(())
    }

    /// Adds `value`, with what it owns counted first; whether it is new.
    /// A value the budget refuses is dropped.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert(&mut self, ledger: Ledger<'_>, value: T) -> Result<bool, Refused>
    where
        T: Owned,
    {
        ledger.open()?;
        let owned = value.owned();
        // A set with room takes it, new or not, without growing, and one
        // that owns nothing without a lookup.
        if owned > 0 || self.0.len() >= self.0.capacity() {
            if self.0.contains(&value) {
                return Ok(false);
            }
            self.room(ledger, 1, owned)?;
        }
        Ok(self.0.insert(value))
    }

    /// Adds `value` in room [`Self::reserve`] made for it, taking what it
    /// owns from what `kept` counted for it; whether it is new.
    pub fn insert_kept(&mut self, kept: &mut Kept, value: T) -> bool
    where
        T: Owned,
    {
        kept.spend(value.owned());
        debug_assert!(
            self.0.len() < self.0.capacity() || self.0.contains(&value),
            "room is made first"
        );
        self.0.insert(value)
    }
}

impl<T> Deref for CountedSet<T> {
    type Target = HashSet<T>;

    fn deref(&self) -> &HashSet<T> {
        &self.0
    }
}

impl<'a, T> IntoIterator for &'a CountedSet<T> {
    type Item = &'a T;
    type IntoIter = std::collections::hash_set::Iter<'a, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.iter()
    }
}

impl<T: Heap> Heap for CountedSet<T> {
    fn heap(&self) -> usize {
        self.0.heap()
    }
}

/// A B-tree set whose growth is counted, as its measure counts its nodes,
/// before it happens. It reads as a set; it grows only through
/// [`Self::insert`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CountedBTreeSet<T>(BTreeSet<T>);

impl<T> Default for CountedBTreeSet<T> {
    fn default() -> Self {
        Self(BTreeSet::new())
    }
}

impl<T: Ord> CountedBTreeSet<T> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds `value`; whether it is new. A new element's share of the
    /// nodes, and what it owns, are counted first; a value the budget
    /// refuses is dropped.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert(&mut self, ledger: Ledger<'_>, value: T) -> Result<bool, Refused>
    where
        T: Owned,
    {
        let owned = value.owned();
        self.add(ledger, value, owned)
    }

    /// Adds `value`, taking what it owns from what `kept` counted for it,
    /// and counting a new element's share of the nodes first; whether it
    /// is new.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert_kept(
        &mut self,
        ledger: Ledger<'_>,
        kept: &mut Kept,
        value: T,
    ) -> Result<bool, Refused>
    where
        T: Owned,
    {
        kept.spend(value.owned());
        self.add(ledger, value, 0)
    }

    /// Adds `value`, counting a new element's share of the nodes and
    /// `owned` bytes first.
    fn add(&mut self, ledger: Ledger<'_>, value: T, owned: usize) -> Result<bool, Refused> {
        ledger.open()?;
        let length = self.0.len();
        let grown = btree_storage::<T>(length + 1) - btree_storage::<T>(length);
        // One that takes no more nodes, and owns nothing, is added, new or
        // not, as it is.
        if grown + owned == 0 {
            return Ok(self.0.insert(value));
        }
        if self.0.contains(&value) {
            return Ok(false);
        }
        let peak = ledger.admit(grown + owned)?;
        let added = self.0.insert(value);
        ledger.grew(grown + owned, peak);
        Ok(added)
    }

    pub fn into_set(self) -> BTreeSet<T> {
        self.0
    }
}

impl<T> Deref for CountedBTreeSet<T> {
    type Target = BTreeSet<T>;

    fn deref(&self) -> &BTreeSet<T> {
        &self.0
    }
}

impl<T: Heap> Heap for CountedBTreeSet<T> {
    fn heap(&self) -> usize {
        self.0.heap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compilation::Budget;
    use std::sync::Arc;

    fn meter(memory: Option<usize>) -> Arc<Meter> {
        Meter::new(
            Budget {
                memory,
                ..Budget::default()
            },
            None,
        )
    }

    #[test]
    fn a_stopped_check_changes_no_table() {
        // A change that takes no more room is refused once the check has
        // stopped, as one that takes room is, so a loop that adds to a
        // table stops with the check.
        let meter = meter(None);
        let mut set = CountedSet::new();
        assert_eq!(set.insert(meter.tables(), 1usize), Ok(true));
        let mut map = CountedMap::new();
        assert!(map.insert(meter.tables(), 1usize, 1usize).is_ok());
        let mut list = CountedVec::new();
        assert!(list.reserve(meter.tables(), 4).is_ok());
        meter.stop();
        assert!(meter.tables().keep(0).is_err());
        assert!(set.insert(meter.tables(), 1).is_err());
        assert!(map.insert(meter.tables(), 1, 2).is_err());
        assert!(list.push(meter.tables(), 1usize).is_err());
        assert_eq!(map.get(&1), Some(&1));
        assert!(list.is_empty());
    }

    #[test]
    fn a_list_counts_its_growth_exactly_and_the_measure_takes_it_back() {
        let meter = meter(None);
        let ledger = Ledger::new(&meter, Side::Tables);
        let mut list = CountedVec::new();
        for value in 0..1_000u32 {
            list.push(ledger, value).unwrap();
        }
        assert_eq!(meter.unmeasured(), super::super::meter::vec(list.as_vec()));
        // Measuring the tables takes their growth back into the measure.
        meter.outside(super::super::meter::vec(list.as_vec()));
        assert_eq!(meter.unmeasured(), 0);
        list.extend_from_slice(ledger, &(0..5_000u32).collect::<Vec<_>>())
            .unwrap();
        assert_eq!(
            meter.unmeasured() + 4 * 1_024,
            super::super::meter::vec(list.as_vec())
        );
    }

    #[test]
    fn a_refused_list_keeps_what_it_had() {
        let meter = meter(Some(4_096));
        let ledger = Ledger::new(&meter, Side::Tables);
        let mut list = CountedVec::new();
        let mut refused = None;
        for value in 0..10_000u32 {
            if list.push(ledger, value).is_err() {
                refused = Some(value);
                break;
            }
        }
        let refused = refused.expect("the budget refuses the list's growth");
        let (length, capacity) = (list.len(), list.capacity());
        assert_eq!(length as u32, refused);
        assert!(meter.stopped());
        // A stopped check grows nothing more, but a list with room takes
        // what fits.
        assert_eq!(list.push(ledger, 0).is_err(), length == capacity);
        list.truncate(length);
        assert_eq!(
            list.extend_from_slice(ledger, &(0..capacity as u32).collect::<Vec<_>>()),
            Err(Refused)
        );
        assert_eq!((list.len(), list.capacity()), (length, capacity));
        assert!(list.iter().copied().eq(0..length as u32));
    }

    #[test]
    fn a_map_and_a_set_count_what_their_measures_count() {
        let meter = meter(None);
        let ledger = Ledger::new(&meter, Side::Types);
        let mut entries = CountedMap::new();
        let mut members = CountedSet::new();
        for key in 0..3_000u32 {
            assert_eq!(entries.insert(ledger, key, u64::from(key)).unwrap(), None);
            assert!(members.insert(ledger, key).unwrap());
        }
        assert_eq!(entries.insert(ledger, 7, 0).unwrap(), Some(7));
        assert!(!members.insert(ledger, 7).unwrap());
        assert_eq!(meter.unmeasured(), map(&*entries) + set(&*members));
        // A measure of the type table takes its side's growth back.
        meter.held(0);
        assert_eq!(meter.unmeasured(), 0);
    }

    #[test]
    fn a_refused_map_or_set_stores_nothing() {
        let meter = meter(Some(2_048));
        let ledger = Ledger::new(&meter, Side::Tables);
        let mut entries = CountedMap::new();
        let mut last = 0;
        for key in 0..10_000u32 {
            if entries.insert(ledger, key, key).is_err() {
                last = key;
                break;
            }
        }
        assert!(last > 0 && meter.stopped());
        assert!(!entries.contains_key(&last));
        assert_eq!(entries.len(), last as usize);
        let mut members = CountedSet::new();
        assert_eq!(members.insert(ledger, 1u32), Err(Refused));
        assert!(members.is_empty() && members.capacity() == 0);
    }

    #[test]
    fn a_btree_set_counts_its_nodes_as_its_measure_does() {
        let meter = meter(None);
        let ledger = Ledger::new(&meter, Side::Declarations);
        let mut names = CountedBTreeSet::new();
        for index in 0..100u32 {
            assert!(names.insert(ledger, index).unwrap());
        }
        assert!(!names.insert(ledger, 3).unwrap());
        assert_eq!(meter.unmeasured(), btree_storage::<u32>(names.len()));
        meter.measured(Side::Declarations);
        assert_eq!(meter.unmeasured(), 0);
        // Payloads are counted before they are made.
        ledger.keep(100).unwrap();
        assert_eq!(meter.unmeasured(), 100);
    }

    #[test]
    fn a_table_counts_what_its_elements_own_before_it_keeps_them() {
        let meter = meter(None);
        let ledger = Ledger::new(&meter, Side::Tables);
        let mut names: CountedVec<String> = CountedVec::new();
        names.reserve(ledger, 8).unwrap();
        let slots = meter.unmeasured();
        names.push(ledger, "x".repeat(1_000)).unwrap();
        assert_eq!(meter.unmeasured(), slots + 1_000);
        let mut map: CountedMap<String, Option<String>> = CountedMap::new();
        map.insert(ledger, "k".repeat(10), Some("v".repeat(20)))
            .unwrap();
        assert_eq!(
            meter.unmeasured(),
            slots + 1_000 + super::super::meter::map(&map) + 30
        );
        // What was counted before a copy is made is taken as it is kept.
        let mut kept = ledger.keep(100).unwrap();
        names.reserve(ledger, 1).unwrap();
        let before = meter.unmeasured();
        names.push_kept(&mut kept, "y".repeat(100));
        assert_eq!(meter.unmeasured(), before);
    }

    #[test]
    fn a_table_that_grows_counts_its_old_storage_once() {
        use super::super::meter::{map, table, vec};
        // The old storage, which the check holds already, stays while the
        // new is taken: a budget with room for both beside the rest admits
        // the growth, though not for the old storage twice.
        let list = |budget: usize| {
            let meter = meter(Some(budget));
            let mut list: CountedVec<u64> = CountedVec::new();
            list.reserve(meter.tables(), 1_024).unwrap();
            for i in 0..1_024 {
                list.push(meter.tables(), i).unwrap();
            }
            (meter, list)
        };
        let (counted, full) = list(usize::MAX);
        let (held, old) = (counted.unmeasured(), vec(full.as_vec()));
        let (counted, mut full) = list(held + 2 * old + old / 2);
        assert!(full.push(counted.tables(), 1_024).is_ok());
        assert!(!counted.stopped());
        let entries = |budget: usize| {
            let meter = meter(Some(budget));
            let mut entries: CountedMap<u64, u64> = CountedMap::new();
            entries.reserve(meter.tables(), 1_024).unwrap();
            for i in 0..entries.capacity() as u64 {
                entries.insert(meter.tables(), i, i).unwrap();
            }
            (meter, entries)
        };
        let (counted, full) = entries(usize::MAX);
        let held = counted.unmeasured();
        let (old, new) = (map(&full.0), table::<(u64, u64)>(full.capacity() + 1));
        let (counted, mut full) = entries(held + new + old / 2);
        assert!(full.insert(counted.tables(), u64::MAX, 0).is_ok());
        assert!(!counted.stopped());
    }

    #[test]
    fn a_table_refuses_an_element_that_owns_more_than_the_budget_leaves() {
        let meter = meter(Some(16 << 10));
        let ledger = Ledger::new(&meter, Side::Tables);
        let mut names: CountedVec<String> = CountedVec::new();
        names.reserve(ledger, 4).unwrap();
        names.push(ledger, "a".repeat(100)).unwrap();
        // The list has room, but not the budget for what the name owns.
        assert_eq!(names.push(ledger, "b".repeat(64 << 10)), Err(Refused));
        assert!(meter.stopped());
        assert_eq!(names.len(), 1);
        let mut set: CountedBTreeSet<String> = CountedBTreeSet::new();
        assert_eq!(set.insert(ledger, "c".repeat(10)), Err(Refused));
        assert!(set.is_empty());
    }

    #[test]
    fn a_scratch_list_gives_back_what_its_elements_own() {
        let meter = meter(None);
        {
            let mut list = ScratchVec::new(&meter);
            list.push("z".repeat(500)).unwrap();
            assert!(meter.unmeasured() >= 500);
        }
        assert_eq!(meter.unmeasured(), 0);
    }

    #[test]
    fn a_scratch_map_gives_back_what_its_entries_own() {
        let meter = meter(None);
        {
            let mut texts = ScratchMap::new(&meter);
            texts.insert(1_u32, "a".repeat(100)).unwrap();
            texts.insert(2, "b".repeat(200)).unwrap();
            // A value given way to gives back what it owned.
            texts.insert(1, "c".repeat(300)).unwrap();
            assert_eq!(texts[&1].len(), 300);
            assert_eq!(
                meter.unmeasured(),
                super::super::meter::map(&texts.map.0) + 500
            );
        }
        assert_eq!(meter.unmeasured(), 0);
    }

    /// The lists and maps in the checker's state that are not tables its
    /// check grows, by the item that holds them and its field: each is made
    /// once at the size it takes and counted before it is kept, or bounded
    /// by the engine rather than the source.
    const UNCOUNTED: &[(&str, &str)] = &[
        // Signatures and enums, made once each as they are declared.
        ("Sig", "params"),
        ("Sig", "vars"),
        ("BlockSig", "params"),
        ("Enum", "members"),
        ("Enum", "symbols"),
        ("Enum", "by_member"),
        ("Enum", "by_symbol"),
        // An indexed union's alternatives by head, made once each.
        ("Types", "index"),
        // A walk's tree of the assignments it listed, and a node of a set
        // of locals, each made at its size.
        ("Root", "lowest"),
        ("Node", "Inner"),
        // Each class's defaults, indexed once and held.
        ("Defaults", "0"),
        // A branch's changes, gathered in a scratch list and held by
        // whoever keeps them; a call's assigned locals, a receiver's
        // types' bases and the session's locals, each made once.
        ("Branch", "changes"),
        ("FileCall", "assigned"),
        ("ReceiverType", "bases"),
        ("Session", "locals"),
        // The engine's hosts, and its builtin signatures once converted.
        ("Required", "hosts"),
        ("Converter", "cache"),
    ];

    /// The lists and maps in the items the checker's state reaches, each
    /// with the item and the field that holds it.
    fn collections_in_state() -> Vec<(String, String, String)> {
        collections(true)
    }

    /// The lists and maps in the checker's items, each with the item and the
    /// field that holds it: those the checker's state reaches, when
    /// `reached`, and the rest otherwise, values its operations build and
    /// drop.
    fn collections(reached: bool) -> Vec<(String, String, String)> {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = vec![root.join("typing.rs")];
        for entry in std::fs::read_dir(root.join("typing")).unwrap() {
            sources.push(entry.unwrap().path());
        }
        // Each struct's and enum's fields, by the item's name; the tables
        // themselves are made here, of lists and maps.
        let mut items: Vec<(String, Vec<(String, String)>)> = Vec::new();
        for path in sources {
            if path.file_name().is_some_and(|name| name == "counted.rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let text = text.split("#[cfg(test)]").next().unwrap();
            let code: String = text
                .lines()
                .map(|line| line.split("//").next().unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            items.extend(fields(&code));
        }
        // What the checker's state reaches, through the types its fields
        // name.
        let mut state_reached = vec!["Checker".to_owned()];
        let mut at = 0;
        while at < state_reached.len() {
            let name = state_reached[at].clone();
            at += 1;
            for (_, fields) in items.iter().filter(|(item, _)| *item == name) {
                for (_, ty) in fields {
                    for word in ty.split(|c: char| !c.is_alphanumeric() && c != '_') {
                        if items.iter().any(|(item, _)| item == word)
                            && !state_reached.iter().any(|seen| seen == word)
                        {
                            state_reached.push(word.to_owned());
                        }
                    }
                }
            }
        }
        let mut found = Vec::new();
        for (item, fields) in items
            .iter()
            .filter(|(item, _)| state_reached.contains(item) == reached)
        {
            for (field, ty) in fields {
                for collection in [
                    "Vec<",
                    "VecDeque<",
                    "HashMap<",
                    "HashSet<",
                    "BTreeMap<",
                    "BTreeSet<",
                ] {
                    let bare = ty.match_indices(collection).any(|(start, _)| {
                        ty[..start]
                            .chars()
                            .next_back()
                            .is_none_or(|c| !c.is_alphanumeric() && c != '_')
                    });
                    if bare {
                        found.push((item.clone(), field.clone(), collection.to_owned()));
                    }
                }
            }
        }
        found
    }

    /// The structs and enums in `code`, each with its fields: a named
    /// field's name, an enum variant's, or a tuple field's place, with its
    /// type, or a variant's fields.
    fn fields(code: &str) -> Vec<(String, Vec<(String, String)>)> {
        let mut found = Vec::new();
        for (keyword, variants) in [("struct ", false), ("enum ", true)] {
            for (start, _) in code.match_indices(keyword) {
                let before = code[..start].chars().next_back();
                if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                    continue;
                }
                let rest = &code[start + keyword.len()..];
                let name: String = rest
                    .chars()
                    .take_while(|c| c.is_alphanumeric() || *c == '_')
                    .collect();
                // The body opens at the first brace or parenthesis past the
                // generics, unless the item ends first.
                let Some(open) = rest.find(['{', '(', ';']) else {
                    continue;
                };
                let opener = rest[open..].chars().next().unwrap();
                if name.is_empty() || opener == ';' {
                    continue;
                }
                let body = &rest[open + 1..open + closing(&rest[open..])];
                let mut parts = Vec::new();
                for (place, part) in split(body).into_iter().enumerate() {
                    let part = part.trim();
                    let part = part.trim_start_matches(|c: char| c == '#' || c.is_whitespace());
                    let label: String = part
                        .trim_start_matches("pub(crate) ")
                        .trim_start_matches("pub(super) ")
                        .trim_start_matches("pub ")
                        .chars()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect();
                    let (label, ty) = match (variants, opener, part.find(':')) {
                        (true, _, _) => (
                            label.clone(),
                            part[label.len().min(part.len())..].to_owned(),
                        ),
                        (false, '{', Some(colon)) => (label, part[colon + 1..].to_owned()),
                        _ => (place.to_string(), part.to_owned()),
                    };
                    if !part.is_empty() {
                        parts.push((label, ty));
                    }
                }
                found.push((name, parts));
            }
        }
        // A type alias is an item of one field, its type.
        for (start, _) in code.match_indices("type ") {
            let before = code[..start].chars().next_back();
            if before.is_some_and(|c| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let rest = &code[start + "type ".len()..];
            let name: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let (Some(equals), Some(end)) = (rest.find('='), rest.find(';')) else {
                continue;
            };
            if !name.is_empty() && equals < end {
                found.push((
                    name,
                    vec![("0".to_owned(), rest[equals + 1..end].to_owned())],
                ));
            }
        }
        found
    }

    /// Where the bracket that opens `text` closes it.
    fn closing(text: &str) -> usize {
        let mut depth = 0;
        for (index, c) in text.char_indices() {
            match c {
                '{' | '(' | '[' | '<' => depth += 1,
                '}' | ')' | ']' | '>' => {
                    depth -= 1;
                    if depth == 0 {
                        return index;
                    }
                }
                _ => (),
            }
        }
        text.len()
    }

    /// `body`'s parts between the commas outside any brackets.
    fn split(body: &str) -> Vec<&str> {
        let mut parts = Vec::new();
        let (mut depth, mut from) = (0, 0);
        for (index, c) in body.char_indices() {
            match c {
                '{' | '(' | '[' | '<' => depth += 1,
                '}' | ')' | ']' | '>' => depth -= 1,
                ',' if depth == 0 => {
                    parts.push(&body[from..index]);
                    from = index + 1;
                }
                _ => (),
            }
        }
        parts.push(&body[from..]);
        parts
    }

    /// The lists and maps in values the checker's operations build and
    /// drop, outside its state, by the item that holds them and its field:
    /// each is counted, held or checked against the budget before it is
    /// made at the most it takes, or bounded by the engine. A value built
    /// of others as an operation recurses, such as a condition's
    /// narrowings, keeps its lists counted while they live, as a scratch
    /// list does, rather than as plain ones only its last step sees.
    const UNCOUNTED_VALUES: &[(&str, &str)] = &[
        // What a check is given, and what it finds, which the checker
        // counts as it finds them and its caller reserves.
        ("Input", "hosts"),
        ("Checked", "diagnostics"),
        ("Checked", "locals"),
        ("CallTypes", "entries"),
        // The reads construction analysis finds for each method, held at
        // the most it takes while it finds them.
        ("Reads", "0"),
        // A call's candidate signatures, a few of one name.
        ("Candidate", "0"),
        // The builtin signatures' index, made once for the engine.
        ("Index", "globals"),
        ("Index", "modules"),
        ("Index", "classes"),
        ("Index", "aliases"),
        ("Index", "renames"),
        // A required file's exports, counted before they are made, and the
        // indexes importing them builds, held while it does.
        ("Exported", "functions"),
        ("Exported", "enums"),
        ("Exported", "classes"),
        ("ExportedClass", "methods"),
        ("Imports", "enums"),
        ("Imports", "classes"),
        // The assignment index walk's stack, whose growth is checked
        // against the budget as it grows.
        ("Pending", "stack"),
    ];

    #[test]
    fn the_checkers_values_keep_their_lists_counted() {
        let found: Vec<String> = collections(false)
            .into_iter()
            .filter(|(item, field, _)| {
                !UNCOUNTED_VALUES
                    .iter()
                    .any(|(allowed, name)| item == allowed && field == name)
            })
            .map(|(item, field, collection)| format!("{item}.{field}: {collection}"))
            .collect();
        assert!(
            found.is_empty(),
            "keep a value's lists counted while they live, or say here why a list or map need not be:\n{}",
            found.join("\n")
        );
    }

    /// The Rust files under `src` of `roots`, each a file or a directory
    /// walked whole, but for those that hold only tests.
    fn sources(roots: &[&str]) -> Vec<std::path::PathBuf> {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut pending: Vec<std::path::PathBuf> =
            roots.iter().map(|root| src.join(root)).collect();
        let mut found = Vec::new();
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                for entry in std::fs::read_dir(&path).unwrap() {
                    pending.push(entry.unwrap().path());
                }
            } else if path.extension().is_some_and(|extension| extension == "rs")
                && !path.ends_with("tests.rs")
                && !path.ends_with("test_support.rs")
            {
                found.push(path);
            }
        }
        found.sort();
        found
    }

    /// The names a walk's stack goes by.
    const STACKS: &[&str] = &[
        "stack", "stacks", "levels", "pending", "frames", "queue", "todo", "trail", "parents",
    ];

    /// The stacks the checker, the parser and the loader keep in plain
    /// lists, by file and name: each made room for before it grows, counted
    /// before the walk starts at the most it takes, or bounded without the
    /// source. A walk's stack holds what is left of each level of what it
    /// walks, which the source nests as deep as the parser allows, so any
    /// other is counted as it grows: a scratch list in the checker, and a
    /// buffer its work reserves in the parser and the loader.
    const UNCOUNTED_STACKS: &[(&str, &str)] = &[
        // The assignment index's walk and the checker's walks, each made
        // room for, with what the walk holds beside it, before it grows.
        ("assigns.rs", "stack"),
        ("walk.rs", "stack"),
        // The cycle search's stacks, counted before the search starts at
        // the most they take, a node each.
        ("construction.rs", "stack"),
        ("construction.rs", "frames"),
        // A set's nodes left to visit: at most 16 for each level of a tree
        // of 16-way nodes over the set's indices, a few levels.
        ("marks.rs", "pending"),
        // Two levels for each interpolation nested in another, which the
        // lexer stops at 8.
        ("record.rs", "levels"),
        // A configured root's path parts, which the host gives, not a
        // script.
        ("root.rs", "pending"),
        // The drop of a syntax tree, which has no budget and frees more
        // than its stack holds, a stack kept only as large as a thread
        // keeps one.
        ("teardown.rs", "pending"),
    ];

    #[test]
    fn the_walks_count_their_stacks() {
        let mut found = Vec::new();
        let mut used = Vec::new();
        for path in sources(&[
            "typing.rs",
            "typing",
            "syntax.rs",
            "syntax",
            "loading.rs",
            "loading",
        ]) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap();
            for line in code.lines().map(str::trim) {
                if line.starts_with("//") {
                    continue;
                }
                for stack in STACKS {
                    let declared =
                        line.strip_prefix("let mut ")
                            .and_then(|rest| rest.strip_prefix(stack))
                            .is_some_and(|rest| {
                                let rest = rest.trim_start();
                                (rest.starts_with(':') || rest.starts_with('='))
                                    && ["vec![", "Vec::", "VecDeque::", "Vec<", "VecDeque<"]
                                        .iter()
                                        .any(|list| {
                                            rest.match_indices(list).any(|(at, _)| {
                                                !rest[..at].chars().next_back().is_some_and(|c| {
                                                    c == '_' || c.is_alphanumeric()
                                                })
                                            })
                                        })
                            });
                    let typed = line.match_indices(&format!("{stack}: ")).any(|(at, _)| {
                        let before = line[..at].chars().next_back();
                        let ty = line[at + stack.len() + 2..].trim_start_matches("&mut ");
                        !before.is_some_and(|c| c == '_' || c.is_alphanumeric())
                            && (ty.starts_with("Vec<") || ty.starts_with("VecDeque<"))
                    });
                    if !(declared || typed) {
                        continue;
                    }
                    let allowed = UNCOUNTED_STACKS
                        .iter()
                        .find(|&&(file, allowed)| file == name && allowed == *stack);
                    if let Some(&entry) = allowed {
                        used.push(entry);
                    } else {
                        found.push(format!("{name}: {line}"));
                    }
                }
            }
        }
        let stale: Vec<String> = UNCOUNTED_STACKS
            .iter()
            .filter(|entry| !used.contains(entry))
            .map(|(file, stack)| format!("{file}: {stack}"))
            .collect();
        assert!(
            found.is_empty() && stale.is_empty(),
            "count a walk's stack as it grows, or say here why it need not be:\n{}\nno longer kept in a plain list:\n{}",
            found.join("\n"),
            stale.join("\n")
        );
    }

    /// What a loop does to find whether the check has stopped, or to stop
    /// it: ask, charge, pace or poll.
    const HALTS: &[&str] = &[
        "halted()",
        "over_budget()",
        "paced(",
        "pace(",
        "charge(",
        "declaring()",
        "declared()",
        "visit()",
        "poll()",
        ".work(",
    ];

    /// What a loop over something the source sizes iterates: the program's
    /// declarations, a required file's exports, a body's statements, the
    /// calls between functions, and the like.
    const SIZED: &[&str] = &[
        "namespaces",
        ".fns",
        "exported.",
        "parsed.",
        "instance_methods",
        ".methods",
        "outline",
        "additions",
        "file_calls",
        ".functions",
        "classes",
        "enums",
        "uses",
        "calls",
        "sites",
        "requests",
        "diagnostics",
        "body",
        "stmts",
        "reads",
        "written",
        "members",
        "fields",
        "entries",
    ];

    /// The loops over what the source sizes that hold another loop and ask
    /// nothing of the budget themselves, by file and header: only what the
    /// engine sizes.
    const UNASKED_LOOPS: &[(&str, &str)] = &[
        // A builtin class's members for a receiver, which the builtin
        // signatures bound.
        ("sigs.rs", "for &class in classes {"),
    ];

    #[test]
    fn every_loop_over_the_source_around_another_asks_whether_the_check_stopped() {
        let is_loop = |line: &str| {
            (line.starts_with("for ") || line.starts_with("while ") || line == "loop {")
                && line.ends_with('{')
        };
        let indent = |line: &str| line.len() - line.trim_start().len();
        let mut found = Vec::new();
        let mut used = Vec::new();
        for path in sources(&["typing.rs", "typing"]) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let text = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<&str> = text.split("#[cfg(test)]").next().unwrap().lines().collect();
            // Where the block a line opens ends: the next line indented no
            // deeper.
            let end = |start: usize| {
                (start + 1..lines.len())
                    .find(|&at| {
                        !lines[at].trim().is_empty() && indent(lines[at]) <= indent(lines[start])
                    })
                    .unwrap_or(lines.len())
            };
            for (start, line) in lines.iter().enumerate() {
                let header = line.trim();
                let Some((_, iterated)) = header
                    .strip_prefix("for ")
                    .and_then(|rest| rest.split_once(" in "))
                else {
                    continue;
                };
                if !header.ends_with('{') || !SIZED.iter().any(|sized| iterated.contains(sized)) {
                    continue;
                }
                let close = end(start);
                let inner: Vec<(usize, usize)> = (start + 1..close)
                    .filter(|&at| is_loop(lines[at].trim()))
                    .map(|at| (at, end(at)))
                    .collect();
                if inner.is_empty() {
                    continue;
                }
                let asks = (start + 1..close)
                    .filter(|&at| !inner.iter().any(|&(from, to)| (from..to).contains(&at)))
                    .any(|at| HALTS.iter().any(|halt| lines[at].contains(halt)));
                if asks {
                    continue;
                }
                let allowed = UNASKED_LOOPS
                    .iter()
                    .find(|&&(file, allowed)| file == name && allowed == header);
                if let Some(&entry) = allowed {
                    used.push(entry);
                } else {
                    found.push(format!("{name}:{}: {header}", start + 1));
                }
            }
        }
        let stale: Vec<String> = UNASKED_LOOPS
            .iter()
            .filter(|entry| !used.contains(entry))
            .map(|(file, header)| format!("{file}: {header}"))
            .collect();
        assert!(
            found.is_empty() && stale.is_empty(),
            "ask whether the check stopped at the top of the outer loop, or say here why it need not:\n{}\nno longer found:\n{}",
            found.join("\n"),
            stale.join("\n")
        );
    }

    #[test]
    fn the_checkers_state_grows_only_through_counted_tables() {
        let found: Vec<String> = collections_in_state()
            .into_iter()
            .filter(|(item, field, _)| {
                !UNCOUNTED
                    .iter()
                    .any(|(allowed, name)| item == allowed && field == name)
            })
            .map(|(item, field, collection)| format!("{item}.{field}: {collection}"))
            .collect();
        assert!(
            found.is_empty(),
            "grow the checker's state through counted tables, or say here why a list or map need not be one:\n{}",
            found.join("\n")
        );
    }

    /// The lists and maps each file of the checker makes for an operation,
    /// with a `.collect()`, `with_capacity`, `to_vec` or `vec!`, by file and
    /// in four kinds: a few elements, a number the code fixes; a type's or
    /// a signature's parts, at most a union's 1,024 alternatives or a
    /// shape's 16,384 fields, which the type table or the declarations
    /// count already, none of which is kept while work that could make
    /// another runs beneath it, so that such lists never pile up through
    /// nesting, aliases or repetition; the builtin signatures', which the
    /// engine bounds; and those counted, held or checked against the
    /// budget before they are made, at the most they take, which they
    /// never grow past. A list the source sizes otherwise, or a type's
    /// parts kept while nested work that makes more runs, is a scratch
    /// list, counted while it lives.
    const MADE: &[(&str, [usize; 4])] = &[
        ("typing.rs", [0, 0, 0, 1]),
        ("assigns.rs", [0, 0, 0, 2]),
        ("calls.rs", [0, 1, 4, 6]),
        ("check.rs", [3, 0, 0, 11]),
        ("construction.rs", [1, 0, 0, 15]),
        ("expr.rs", [2, 6, 0, 10]),
        ("flow.rs", [0, 0, 0, 6]),
        ("foreign.rs", [1, 0, 0, 0]),
        ("marks.rs", [2, 0, 0, 1]),
        ("modules.rs", [0, 0, 0, 9]),
        ("program.rs", [0, 0, 0, 14]),
        ("sigs.rs", [0, 1, 9, 0]),
        ("ty.rs", [10, 2, 0, 1]),
    ];

    #[test]
    fn the_lists_an_operation_makes_are_each_of_a_kind() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = vec![root.join("typing.rs")];
        for entry in std::fs::read_dir(root.join("typing")).unwrap() {
            sources.push(entry.unwrap().path());
        }
        let mut found = Vec::new();
        for path in sources {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name == "counted.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap();
            let made = code
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .filter(|line| {
                    [".collect(", "with_capacity(", ".to_vec()", "vec!["]
                        .iter()
                        .any(|site| line.contains(site))
                })
                .count();
            let kinds = MADE
                .iter()
                .find(|(file, _)| *file == name)
                .map_or(0, |(_, kinds)| kinds.iter().sum());
            if made != kinds {
                found.push(format!("{name}: {made} made, {kinds} of a kind"));
            }
        }
        assert!(
            found.is_empty(),
            "say here which kind each list or map these files make is, or make it in a scratch list:\n{}",
            found.join("\n")
        );
    }

    /// The empty lists, maps, sets and strings each file of the checker
    /// starts, with a `Vec::new()`, `HashMap::new()`, `String::new()` or
    /// the like, or an `.or_default()`, which then grow by `push` or
    /// `insert` where [`MADE`] does not see them, by file and in three
    /// kinds: one returned, passed or kept empty, or one a field starts
    /// with and a counted table replaces whole, which nothing grows here;
    /// one bounded without the source: a few of the builtin signatures' or
    /// the host's, a type's display, whose parts share `SPELLED` bytes
    /// however deep it nests, or a union's alternatives, at most 1,024,
    /// none of which is kept while work that could make another runs
    /// beneath it, so that such lists never pile up through nesting,
    /// aliases or repetition; and one counted before it is made at the
    /// most it takes, with the room its growth makes and the old room it
    /// moves from, or paced by a walk with its capacity. A hold, a `keep`
    /// or a check of each element as the list takes it is none of these:
    /// it counts the elements, not the spare room a list grows by or the
    /// moment it moves, so a list that grows under one is a scratch list.
    /// A list the source sizes otherwise, or one of a type's parts kept
    /// while nested work that makes more runs, is a scratch list too,
    /// counted while it lives.
    const STARTED: &[(&str, [usize; 3])] = &[
        // The frame's name, and the results of a stopped check.
        ("typing.rs", [6, 0, 0]),
        // A stopped walk's results, and a root without assignments.
        ("assigns.rs", [3, 0, 0]),
        // Bindings of signatures without type variables, and the members
        // of a host or builtin namespace of one name, a few.
        ("calls.rs", [21, 2, 0]),
        // A union's alternatives an `is_a?` keeps, and the defaults by
        // class, counted before they are indexed at the most they take.
        ("check.rs", [8, 1, 2]),
        // The cycle search's stack, cycles and their members, counted
        // before the search starts.
        ("construction.rs", [10, 0, 3]),
        // A hint's enums, and the results of an operator's alternatives,
        // which relate types but check no syntax.
        ("expr.rs", [1, 2, 0]),
        // A stopped check's branches.
        ("flow.rs", [4, 0, 0]),
        // The size of an empty node.
        ("marks.rs", [1, 0, 0]),
        // A namespace's signature without type variables.
        ("modules.rs", [1, 0, 0]),
        // A signature's variables.
        ("program.rs", [1, 0, 0]),
        // The builtin signatures' index, and a signature's variables.
        ("sigs.rs", [1, 10, 0]),
        // A display's text and parts, which share one room; a union's
        // index, counted before it is built.
        ("ty.rs", [5, 5, 2]),
        // The walk's stack, paced with what the walker holds.
        ("walk.rs", [0, 0, 1]),
    ];

    /// How many empty lists, maps, sets and strings `line` starts: those
    /// [`STARTED`] lists, and not the counted ones, such as a
    /// `ScratchVec::new`.
    fn started(line: &str) -> usize {
        let mut count = line.matches(".or_default()").count();
        for kind in [
            "Vec", "HashMap", "HashSet", "BTreeMap", "BTreeSet", "String",
        ] {
            for made in ["::new()", "::default()"] {
                let site = format!("{kind}{made}");
                count += line
                    .match_indices(&site)
                    .filter(|&(at, _)| {
                        !line[..at]
                            .chars()
                            .next_back()
                            .is_some_and(|c| c == '_' || c.is_alphanumeric())
                    })
                    .count();
            }
        }
        count
    }

    #[test]
    fn the_empty_lists_an_operation_starts_are_each_of_a_kind() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = vec![root.join("typing.rs")];
        for entry in std::fs::read_dir(root.join("typing")).unwrap() {
            sources.push(entry.unwrap().path());
        }
        let mut found = Vec::new();
        for path in sources {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name == "counted.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap();
            let count: usize = code
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .map(started)
                .sum();
            let kinds = STARTED
                .iter()
                .find(|(file, _)| *file == name)
                .map_or(0, |(_, kinds)| kinds.iter().sum());
            if count != kinds {
                found.push(format!("{name}: {count} started, {kinds} of a kind"));
            }
        }
        assert!(
            found.is_empty(),
            "say here which kind each empty list or map these files start is, or start it as a scratch list:\n{}",
            found.join("\n")
        );
    }

    #[test]
    fn the_lint_of_empty_lists_skips_counted_ones() {
        assert_eq!(started("let mut kept = Vec::new();"), 1);
        assert_eq!(started("(Vec::new(), Vec::new(), false)"), 2);
        assert_eq!(started("let names = std::collections::HashSet::new();"), 1);
        assert_eq!(started("map.entry(key).or_default().push(value);"), 1);
        assert_eq!(started("let kept = ScratchVec::new(&self.meter);"), 0);
        assert_eq!(started("let kept = CountedVec::new();"), 0);
        assert_eq!(started("let mut out = String::default();"), 1);
    }

    /// The text each file of the checker writes without the meter, with a
    /// `format!`, `to_string`, `to_owned` or `String::from`, by file and in
    /// three kinds: a fixed word or a number, which is short; a copy of a
    /// name counted before it is made, which a table then takes, or which
    /// is held while it lives; and a copy of a name a counted table takes,
    /// which it counts as it takes it. Any other text, and a diagnostic's
    /// above all, is written through the meter with `text!` or `copy`.
    const WRITTEN: &[(&str, [usize; 3])] = &[
        ("calls.rs", [2, 0, 0]),
        ("check.rs", [2, 6, 1]),
        ("construction.rs", [0, 1, 0]),
        ("expr.rs", [1, 0, 0]),
        ("modules.rs", [0, 2, 2]),
        ("program.rs", [0, 2, 6]),
        ("ty.rs", [3, 0, 0]),
    ];

    #[test]
    fn the_text_the_checker_writes_is_metered_or_of_a_kind() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = vec![root.join("typing.rs")];
        for entry in std::fs::read_dir(root.join("typing")).unwrap() {
            sources.push(entry.unwrap().path());
        }
        let mut found = Vec::new();
        for path in sources {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name == "counted.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap();
            let written = code
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .filter(|line| {
                    ["format!(", ".to_string()", ".to_owned()", "String::from("]
                        .iter()
                        .any(|site| line.contains(site))
                })
                .count();
            let kinds = WRITTEN
                .iter()
                .find(|(file, _)| *file == name)
                .map_or(0, |(_, kinds)| kinds.iter().sum());
            if written != kinds {
                found.push(format!("{name}: {written} written, {kinds} of a kind"));
            }
        }
        assert!(
            found.is_empty(),
            "write the checker's text through the meter, or say here which kind it is:\n{}",
            found.join("\n")
        );
    }

    /// The copies each file of the checker makes in the arguments of an
    /// `insert`, a `push` or an `add`, or of a `_regardless` one, with a
    /// `to_owned`, `to_string`, `clone`, `format!`, `String::from` or
    /// `to_vec`, by file and in three kinds: into a list or a set counted,
    /// with the copy, before it is made; into a table whose entries have
    /// fixed places, which it keeps however the budget stands; and a fixed
    /// word, or one of the builtin signatures', which the engine bounds.
    /// Any other copy a counted table takes is counted, with its room,
    /// before it is made, as `insert_made`, `push_made` and `reserve_with`
    /// count it, and is not made once the budget refuses it.
    const COPIED: &[(&str, [usize; 3])] = &[
        // The unassigned variables listed, and the reads a caller gathers;
        // a fix's closing bracket.
        ("check.rs", [2, 0, 1]),
        // A required file's classes, signatures and fields, which its
        // importer counts first.
        ("modules.rs", [3, 0, 0]),
        // A function's parameters; the builtin namespaces', and the
        // enums' and their names, which keep their places.
        ("program.rs", [1, 4, 0]),
        // The builtin signatures' variables, names and bindings.
        ("sigs.rs", [0, 0, 6]),
    ];

    /// How many calls in `code` take a copy in their arguments, as
    /// [`COPIED`] counts them.
    fn copied(code: &str) -> usize {
        let calls = [
            ".insert(",
            ".push(",
            ".add(",
            ".insert_regardless(",
            ".push_regardless(",
            ".get_or_insert_with(",
        ];
        let copies = [
            ".to_owned()",
            ".to_string()",
            ".clone()",
            "format!(",
            "String::from(",
            ".to_vec()",
        ];
        let mut count = 0;
        for call in calls {
            for (at, _) in code.match_indices(call) {
                let start = at + call.len();
                let mut depth = 1;
                let mut end = code.len();
                for (offset, c) in code[start..].char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = start + offset;
                                break;
                            }
                        }
                        _ => (),
                    }
                }
                if copies.iter().any(|copy| code[start..end].contains(copy)) {
                    count += 1;
                }
            }
        }
        count
    }

    #[test]
    fn the_checker_counts_a_copy_before_a_table_takes_it() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = vec![root.join("typing.rs")];
        for entry in std::fs::read_dir(root.join("typing")).unwrap() {
            sources.push(entry.unwrap().path());
        }
        let mut found = Vec::new();
        for path in sources {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name == "counted.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let code: String = text
                .split("#[cfg(test)]")
                .next()
                .unwrap()
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .flat_map(|line| [line, "\n"])
                .collect();
            let count = copied(&code);
            let kinds = COPIED
                .iter()
                .find(|(file, _)| *file == name)
                .map_or(0, |(_, kinds)| kinds.iter().sum());
            if count != kinds {
                found.push(format!("{name}: {count} copies taken, {kinds} of a kind"));
            }
        }
        assert!(
            found.is_empty(),
            "count a copy before a table takes it, or say here which kind it is:\n{}",
            found.join("\n")
        );
    }

    #[test]
    fn the_lint_of_copies_finds_them_in_a_calls_arguments() {
        assert_eq!(copied("map.insert(ledger, name.to_owned(), id)"), 1);
        assert_eq!(
            copied("list\n    .push(\n        tables,\n        f(x.clone()),\n    )"),
            1
        );
        assert_eq!(
            copied("map.insert_made(ledger, n, || name.to_owned(), id)"),
            0
        );
        assert_eq!(copied("list.push(ledger, Rc::clone(&sig))"), 0);
    }

    /// The sorts each file of the checker makes itself rather than through
    /// [`sort_unstable_by`] or [`sort_by`], by file: each sorts a few
    /// elements, such as a union's alternatives, at most 1,024, or charges
    /// its steps first itself, as an enum's members are.
    const SORTED: &[(&str, usize)] = &[("program.rs", 1), ("ty.rs", 1)];

    #[test]
    fn the_checker_sorts_through_the_meter() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = vec![root.join("typing.rs")];
        for entry in std::fs::read_dir(root.join("typing")).unwrap() {
            sources.push(entry.unwrap().path());
        }
        let mut found = Vec::new();
        for path in sources {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if name == "counted.rs" {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let code = text.split("#[cfg(test)]").next().unwrap();
            let sorted = code
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .filter(|line| {
                    [
                        ".sort(",
                        ".sort_by(",
                        ".sort_by_key(",
                        ".sort_unstable(",
                        ".sort_unstable_by(",
                        ".sort_unstable_by_key(",
                    ]
                    .iter()
                    .any(|site| line.contains(site))
                })
                .count();
            let allowed = SORTED
                .iter()
                .find(|(file, _)| *file == name)
                .map_or(0, |(_, count)| *count);
            if sorted != allowed {
                found.push(format!("{name}: {sorted} sorts, {allowed} allowed"));
            }
        }
        assert!(
            found.is_empty(),
            "sort through the meter, or say here why a sort need not be:\n{}",
            found.join("\n")
        );
    }
}
