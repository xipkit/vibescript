//! The checker's tables, which count their growth before it happens.
//!
//! A table the checker keeps is measured with the rest at each of the
//! checker's measures. Between measures it may grow, and a table that grows
//! by a source-sized amount could otherwise pass the memory left before the
//! next measure saw it. Each table here, before it takes more storage, asks
//! the meter whether the check can hold its old and new storage at once,
//! beside what the check held when last measured and what its tables grew
//! since, and records what it grew by, which the next measure of its side
//! takes back into its own count. A table the budget refuses to grow keeps
//! what it had and stores nothing, and its caller stops.

// The checker's tables move onto these one at a time.
#![allow(dead_code)]

use super::meter::{Heap, Meter, Side, btree_storage, map, set, table};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    hash::Hash,
    mem::size_of,
    ops::Deref,
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
    /// name it copies, before they are made.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn keep(self, bytes: usize) -> Result<(), Refused> {
        let peak = self.admit(bytes)?;
        self.grew(bytes, peak);
        Ok(())
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
}

/// A list whose growth is counted before it happens. It reads as a slice;
/// it grows only through the methods that take a [`Ledger`].
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
        let capacity = self.0.capacity();
        let needed = self.0.len().saturating_add(additional);
        if needed <= capacity {
            return Ok(());
        }
        let target = needed.max(2 * capacity).max(4);
        let size = size_of::<T>();
        let peak = ledger.admit((capacity + target).saturating_mul(size))?;
        self.0.reserve_exact(target - self.0.len());
        // The allocator is asked for exactly this much; anything more it
        // gives is counted too.
        ledger.grew((self.0.capacity() - capacity) * size, peak);
        Ok(())
    }

    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn push(&mut self, ledger: Ledger<'_>, value: T) -> Result<(), Refused> {
        self.reserve(ledger, 1)?;
        self.0.push(value);
        Ok(())
    }

    /// Adds `value` however the budget stands, counting what the list
    /// grows by, for a list whose entries have fixed places that must be
    /// kept; the next measure reads a stop.
    pub fn push_regardless(&mut self, ledger: Ledger<'_>, value: T) {
        // A ledger that records regardless refuses nothing.
        match self.reserve(ledger.regardless(), 1) {
            Ok(()) | Err(Refused) => self.0.push(value),
        }
    }

    /// Adds `value` in room [`Self::reserve`] made for it, so that a
    /// caller that changes several tables can count them all before it
    /// changes any.
    pub fn push_within(&mut self, value: T) {
        debug_assert!(self.0.len() < self.0.capacity(), "room is made first");
        self.0.push(value);
    }

    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert(&mut self, ledger: Ledger<'_>, index: usize, value: T) -> Result<(), Refused> {
        self.reserve(ledger, 1)?;
        self.0.insert(index, value);
        Ok(())
    }

    /// Adds every element of `values`, making room for all of them first.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn extend<I>(&mut self, ledger: Ledger<'_>, values: I) -> Result<(), Refused>
    where
        I: IntoIterator<Item = T>,
        I::IntoIter: ExactSizeIterator,
    {
        let values = values.into_iter();
        self.reserve(ledger, values.len())?;
        self.0.extend(values);
        Ok(())
    }

    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn extend_from_slice(&mut self, ledger: Ledger<'_>, values: &[T]) -> Result<(), Refused>
    where
        T: Clone,
    {
        self.reserve(ledger, values.len())?;
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

    pub fn remove(&mut self, index: usize) -> T {
        self.0.remove(index)
    }

    pub fn retain(&mut self, keep: impl FnMut(&T) -> bool) {
        self.0.retain(keep);
    }

    pub fn dedup(&mut self)
    where
        T: PartialEq,
    {
        self.0.dedup();
    }

    pub fn drain(&mut self, range: impl std::ops::RangeBounds<usize>) -> std::vec::Drain<'_, T> {
        self.0.drain(range)
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
        let capacity = self.0.capacity();
        let needed = self.0.len().saturating_add(additional);
        if needed <= capacity {
            return Ok(());
        }
        let before = map(&self.0);
        let most = table::<(K, V)>(needed.max(capacity + 1));
        let peak = ledger.admit(before.saturating_add(most))?;
        self.0.reserve(additional);
        ledger.grew(map(&self.0).saturating_sub(before), peak);
        Ok(())
    }

    /// Stores `value` under `key`, giving back the value it replaces.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert(&mut self, ledger: Ledger<'_>, key: K, value: V) -> Result<Option<V>, Refused> {
        if let Some(slot) = self.0.get_mut(&key) {
            return Ok(Some(std::mem::replace(slot, value)));
        }
        self.reserve(ledger, 1)?;
        Ok(self.0.insert(key, value))
    }

    /// Stores `value` under `key` in room [`Self::reserve`] made for it,
    /// giving back the value it replaces.
    pub fn insert_within(&mut self, key: K, value: V) -> Option<V> {
        debug_assert!(
            self.0.len() < self.0.capacity() || self.0.contains_key(&key),
            "room is made first"
        );
        self.0.insert(key, value)
    }

    /// Stores `value` under `key` however the budget stands, counting what
    /// the map grows by, for a map whose entries must all be kept; the
    /// next measure reads a stop.
    pub fn insert_regardless(&mut self, ledger: Ledger<'_>, key: K, value: V) {
        // A ledger that records regardless refuses nothing.
        match self.reserve(ledger.regardless(), 1) {
            Ok(()) | Err(Refused) => {
                self.0.insert(key, value);
            }
        }
    }

    /// The value under `key`, made by `make` first if the map has none.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn get_or_insert_with(
        &mut self,
        ledger: Ledger<'_>,
        key: K,
        make: impl FnOnce() -> V,
    ) -> Result<&mut V, Refused> {
        if !self.0.contains_key(&key) {
            self.reserve(ledger, 1)?;
        }
        Ok(self.0.entry(key).or_insert_with(make))
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

    pub fn retain(&mut self, keep: impl FnMut(&K, &mut V) -> bool) {
        self.0.retain(keep);
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn values_mut(&mut self) -> std::collections::hash_map::ValuesMut<'_, K, V> {
        self.0.values_mut()
    }

    pub fn iter_mut(&mut self) -> std::collections::hash_map::IterMut<'_, K, V> {
        self.0.iter_mut()
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
        let capacity = self.0.capacity();
        let needed = self.0.len().saturating_add(additional);
        if needed <= capacity {
            return Ok(());
        }
        let before = set(&self.0);
        let most = table::<T>(needed.max(capacity + 1));
        let peak = ledger.admit(before.saturating_add(most))?;
        self.0.reserve(additional);
        ledger.grew(set(&self.0).saturating_sub(before), peak);
        Ok(())
    }

    /// Adds `value`; whether it is new.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert(&mut self, ledger: Ledger<'_>, value: T) -> Result<bool, Refused> {
        if self.0.contains(&value) {
            return Ok(false);
        }
        self.reserve(ledger, 1)?;
        Ok(self.0.insert(value))
    }

    /// Adds `value` in room [`Self::reserve`] made for it; whether it is
    /// new.
    pub fn insert_within(&mut self, value: T) -> bool {
        debug_assert!(
            self.0.len() < self.0.capacity() || self.0.contains(&value),
            "room is made first"
        );
        self.0.insert(value)
    }

    pub fn remove<Q>(&mut self, value: &Q) -> bool
    where
        T: std::borrow::Borrow<Q>,
        Q: Hash + Eq + ?Sized,
    {
        self.0.remove(value)
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    pub fn into_set(self) -> HashSet<T> {
        self.0
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
    /// nodes is counted first; its payload is its caller's to count.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn insert(&mut self, ledger: Ledger<'_>, value: T) -> Result<bool, Refused> {
        if self.0.contains(&value) {
            return Ok(false);
        }
        let length = self.0.len();
        let grown = btree_storage::<T>(length + 1) - btree_storage::<T>(length);
        let peak = ledger.admit(grown)?;
        let added = self.0.insert(value);
        ledger.grew(grown, peak);
        Ok(added)
    }

    pub fn remove<Q>(&mut self, value: &Q) -> bool
    where
        T: std::borrow::Borrow<Q>,
        Q: Ord + ?Sized,
    {
        self.0.remove(value)
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
        list.extend(ledger, 0..5_000u32).unwrap();
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
        assert_eq!(list.extend(ledger, 0..capacity as u32), Err(Refused));
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
}
