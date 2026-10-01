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
            let target = needed.max(2 * capacity).max(16);
            if self.ledger.admit(capacity.saturating_add(target)).is_err() {
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
        let target = needed.max(2 * capacity).max(4);
        let size = size_of::<T>();
        let moment = (capacity + target)
            .saturating_mul(size)
            .saturating_add(owned);
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
        let most = table::<(K, V)>(needed.max(capacity + 1));
        let peak = ledger.admit(before.saturating_add(most).saturating_add(owned))?;
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
        let most = table::<T>(needed.max(capacity + 1));
        let peak = ledger.admit(before.saturating_add(most).saturating_add(owned))?;
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
        let mut reached = vec!["Checker".to_owned()];
        let mut at = 0;
        while at < reached.len() {
            let name = reached[at].clone();
            at += 1;
            for (_, fields) in items.iter().filter(|(item, _)| *item == name) {
                for (_, ty) in fields {
                    for word in ty.split(|c: char| !c.is_alphanumeric() && c != '_') {
                        if items.iter().any(|(item, _)| item == word)
                            && !reached.iter().any(|seen| seen == word)
                        {
                            reached.push(word.to_owned());
                        }
                    }
                }
            }
        }
        let mut found = Vec::new();
        for (item, fields) in items.iter().filter(|(item, _)| reached.contains(item)) {
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
    /// in four kinds: a few elements, or one for each level of syntax the
    /// parser allows; a type's or a signature's parts, which the type table
    /// or the declarations count already; the builtin signatures', which
    /// the engine bounds; and those counted, held or checked against the
    /// budget before they are made, at the lengths they take. A list the
    /// source sizes otherwise is a scratch list, counted while it lives.
    const MADE: &[(&str, [usize; 4])] = &[
        ("typing.rs", [0, 0, 0, 1]),
        ("assigns.rs", [0, 0, 0, 2]),
        ("calls.rs", [0, 3, 4, 5]),
        ("check.rs", [4, 1, 0, 10]),
        ("construction.rs", [1, 0, 0, 15]),
        ("expr.rs", [3, 5, 0, 13]),
        ("flow.rs", [0, 0, 0, 6]),
        ("foreign.rs", [1, 0, 0, 0]),
        ("marks.rs", [2, 0, 0, 1]),
        ("modules.rs", [0, 5, 0, 7]),
        ("program.rs", [0, 0, 0, 15]),
        ("sigs.rs", [0, 1, 9, 0]),
        ("ty.rs", [11, 11, 0, 1]),
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

    /// The text each file of the checker writes without the meter, with a
    /// `format!`, `to_string`, `to_owned` or `String::from`, by file and in
    /// three kinds: a fixed word or a number, which is short; a copy of a
    /// name counted before it is made, which a table then takes, or which
    /// is held while it lives; and a copy of a name a counted table takes,
    /// which it counts as it takes it. Any other text, and a diagnostic's
    /// above all, is written through the meter with `text!` or `copy`.
    const WRITTEN: &[(&str, [usize; 3])] = &[
        ("calls.rs", [2, 0, 0]),
        ("check.rs", [2, 5, 0]),
        ("construction.rs", [0, 1, 0]),
        ("expr.rs", [2, 0, 0]),
        ("modules.rs", [0, 2, 2]),
        ("program.rs", [0, 2, 6]),
        ("ty.rs", [11, 0, 0]),
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

    /// The sorts each file of the checker makes itself rather than through
    /// [`sort_unstable_by`] or [`sort_by`], by file: each sorts a few
    /// elements, such as a union's alternatives, at most 1,024, or charges
    /// its steps first itself, as an enum's members are.
    const SORTED: &[(&str, usize)] = &[("program.rs", 1), ("ty.rs", 2)];

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
