use super::{Buffer, Name, Work};
use crate::Result;
use std::{collections::hash_map::DefaultHasher, hash::Hasher};

#[derive(Debug)]
enum Bucket<V> {
    Empty,
    Deleted,
    Entry { hash: u64, name: Name, value: V },
}

#[derive(Debug)]
pub(crate) struct Table<V> {
    buckets: Buffer<Bucket<V>>,
    len: usize,
    deleted: usize,
}

impl<V> Default for Table<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> Table<V> {
    /// Creates an empty compiler table without allocating buckets.
    pub fn new() -> Self {
        Self {
            buckets: Buffer::new(),
            len: 0,
            deleted: 0,
        }
    }

    fn hash(work: &dyn Work, name: &str) -> Result<u64> {
        work.checkpoint()?;
        work.charge(1)?;
        // Fixed keys keep compilation counters independent of random table seeds.
        let mut hasher = DefaultHasher::new();
        for chunk in name.as_bytes().chunks(4096) {
            work.bytes(chunk.len())?;
            hasher.write(chunk);
        }
        Ok(hasher.finish())
    }

    fn same_name(work: &dyn Work, left: &str, right: &str) -> Result<bool> {
        for (left, right) in left
            .as_bytes()
            .chunks(4096)
            .zip(right.as_bytes().chunks(4096))
        {
            work.bytes(left.len())?;
            if left != right {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn find(&self, work: &dyn Work, name: &str, hash: u64) -> Result<(usize, bool)> {
        if self.buckets.is_empty() {
            return Ok((0, false));
        }
        let mask = self.buckets.len() - 1;
        let mut slot = hash as usize & mask;
        let mut deleted = None;
        loop {
            work.charge(1)?;
            match &self.buckets[slot] {
                Bucket::Empty => return Ok((deleted.unwrap_or(slot), false)),
                Bucket::Deleted => {
                    deleted.get_or_insert(slot);
                }
                Bucket::Entry {
                    hash: existing,
                    name: key,
                    ..
                } => {
                    if hash == *existing
                        && name.len() == key.len()
                        && Self::same_name(work, name, key)?
                    {
                        return Ok((slot, true));
                    }
                }
            }
            slot = (slot + 1) & mask;
        }
    }

    /// Finds a binding while charging hashing, probes and spelling comparisons.
    pub fn get(&self, work: &dyn Work, name: &str) -> Result<Option<&V>> {
        let (slot, found) = self.find(work, name, Self::hash(work, name)?)?;
        Ok(if found {
            let Bucket::Entry { value, .. } = &self.buckets[slot] else {
                unreachable!()
            };
            Some(value)
        } else {
            None
        })
    }

    /// Reports whether the table has no bindings, without charging work.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Checks membership with the same termination checks as a value lookup.
    pub fn contains(&self, work: &dyn Work, name: &str) -> Result<bool> {
        Ok(self.get(work, name)?.is_some())
    }

    /// Removes a binding after its lookup succeeds, retaining reusable bucket capacity.
    pub fn remove(&mut self, work: &dyn Work, name: &str) -> Result<Option<V>> {
        let (slot, found) = self.find(work, name, Self::hash(work, name)?)?;
        if !found {
            return Ok(None);
        }
        let Bucket::Entry { value, .. } =
            std::mem::replace(&mut self.buckets[slot], Bucket::Deleted)
        else {
            unreachable!()
        };
        self.len -= 1;
        self.deleted += 1;
        Ok(Some(value))
    }

    /// Charges the bucket scan before exposing borrowed entries.
    pub fn iter(&self, work: &dyn Work) -> Result<impl Iterator<Item = (&Name, &V)>> {
        work.checkpoint()?;
        work.charge(self.buckets.len())?;
        Ok(self.buckets.iter().filter_map(|bucket| match bucket {
            Bucket::Entry { name, value, .. } => Some((name, value)),
            _ => None,
        }))
    }
}

impl<V: Copy> Table<V> {
    fn vacant(buckets: &[Bucket<V>], work: &dyn Work, hash: u64) -> Result<usize> {
        let mask = buckets.len() - 1;
        let mut slot = hash as usize & mask;
        loop {
            work.charge(1)?;
            if matches!(buckets[slot], Bucket::Empty) {
                return Ok(slot);
            }
            slot = (slot + 1) & mask;
        }
    }

    fn rebuild(&self, work: &dyn Work, capacity: usize) -> Result<Buffer<Bucket<V>>> {
        let mut buckets = Buffer::with_capacity(work, capacity)?;
        for _ in 0..capacity {
            work.charge(1)?;
            buckets.push(work, Bucket::Empty)?;
        }
        for bucket in &self.buckets {
            work.charge(1)?;
            if let Bucket::Entry { hash, name, value } = bucket {
                let slot = Self::vacant(&buckets, work, *hash)?;
                buckets[slot] = Bucket::Entry {
                    hash: *hash,
                    name: name.clone(),
                    value: *value,
                };
            }
        }
        Ok(buckets)
    }

    /// Inserts or replaces a binding, preserving the table if reservation or work fails.
    pub fn insert(&mut self, work: &dyn Work, name: Name, value: V) -> Result<Option<V>> {
        let hash = Self::hash(work, &name)?;
        let (slot, found) = self.find(work, &name, hash)?;
        if found {
            let Bucket::Entry {
                value: previous, ..
            } = &mut self.buckets[slot]
            else {
                unreachable!()
            };
            return Ok(Some(std::mem::replace(previous, value)));
        }
        let reuse = !self.buckets.is_empty() && matches!(self.buckets[slot], Bucket::Deleted);
        if !reuse && self.len + self.deleted + 1 > self.buckets.len() / 2 {
            let capacity = if self.buckets.is_empty() {
                8
            } else if self.len + 1 > self.buckets.len() / 2 {
                self.buckets
                    .len()
                    .checked_mul(2)
                    .ok_or_else(|| work.allocation_error("compiler table size overflow"))?
            } else {
                self.buckets.len()
            };
            let mut buckets = self.rebuild(work, capacity)?;
            let slot = Self::vacant(&buckets, work, hash)?;
            buckets[slot] = Bucket::Entry { hash, name, value };
            self.buckets = buckets;
            self.deleted = 0;
        } else {
            if reuse {
                self.deleted -= 1;
            }
            self.buckets[slot] = Bucket::Entry { hash, name, value };
        }
        self.len += 1;
        Ok(None)
    }

    /// Copies table storage while sharing immutable, already-accounted spellings.
    pub fn copy(&self, work: &dyn Work) -> Result<Self> {
        let buckets = self.buckets.copy_with(work, |bucket| {
            Ok(match bucket {
                Bucket::Empty => Bucket::Empty,
                Bucket::Deleted => Bucket::Deleted,
                Bucket::Entry { hash, name, value } => Bucket::Entry {
                    hash: *hash,
                    name: name.clone(),
                    value: *value,
                },
            })
        })?;
        Ok(Self {
            buckets,
            len: self.len,
            deleted: self.deleted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallContext, CallOptions, ErrorKind, compilation::Meter};
    use std::{cell::RefCell, collections::BTreeMap, sync::Arc, time::Instant};

    fn names(work: &dyn Work, count: usize) -> Vec<Name> {
        (0..count)
            .map(|i| Name::new(work, &format!("name_{i}")).unwrap())
            .collect()
    }

    #[test]
    fn collisions_wrap_and_deleted_slots_are_reused_without_growth() {
        let mut context = CallContext::new(CallOptions::default());
        let work = Meter(RefCell::new(&mut context));
        let names: Vec<_> = (0..)
            .map(|i| format!("collision_{i}"))
            .filter(|name| Table::<usize>::hash(&(), name).unwrap() & 7 == 7)
            .take(6)
            .map(|name| Name::new(&work, &name).unwrap())
            .collect();
        let mut table = Table::new();
        for (i, name) in names.iter().take(4).enumerate() {
            assert_eq!(table.insert(&work, name.clone(), i).unwrap(), None);
        }
        let before = work.0.borrow().stats().retained_memory_bytes;
        work.0.borrow_mut().options.limits.memory_bytes = Some(before);
        assert_eq!(table.remove(&work, &names[1]).unwrap(), Some(1));
        assert_eq!(table.get(&work, &names[3]).unwrap(), Some(&3));
        assert_eq!(table.insert(&work, names[4].clone(), 4).unwrap(), None);
        assert_eq!(table.deleted, 0);
        assert_eq!(table.insert(&work, names[0].clone(), 99).unwrap(), Some(0));
        assert_eq!(table.get(&work, &names[0]).unwrap(), Some(&99));
        assert_eq!(table.get(&work, &names[1]).unwrap(), None);
        assert_eq!(table.get(&work, &names[5]).unwrap(), None);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, before);
        drop(table);
        drop(names);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
    }

    #[test]
    fn growth_reserves_both_tables_and_preserves_the_original_on_failure() {
        for shortage in [0, 1] {
            let mut context = CallContext::new(CallOptions::default());
            let work = Meter(RefCell::new(&mut context));
            let names = names(&work, 5);
            let mut table = Table::new();
            for (i, name) in names.iter().take(4).enumerate() {
                table.insert(&work, name.clone(), i).unwrap();
            }
            let old_storage = table.buckets.len() * size_of::<Bucket<usize>>();
            let before = work.0.borrow().stats().retained_memory_bytes;
            work.0.borrow_mut().options.limits.memory_bytes =
                Some(before + 2 * old_storage - shortage);
            let result = table.insert(&work, names[4].clone(), 4);
            if shortage == 0 {
                assert_eq!(result.unwrap(), None);
                assert_eq!(
                    work.0.borrow().stats().retained_memory_bytes,
                    before + old_storage
                );
                assert_eq!(
                    work.0.borrow().stats().peak_memory_bytes,
                    before + 2 * old_storage
                );
            } else {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(work.checkpoint().unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(table.len, 4);
                assert_eq!(table.get(&(), &names[4]).unwrap(), None);
                assert_eq!(work.0.borrow().stats().retained_memory_bytes, before);
            }
            for (i, name) in names.iter().take(4).enumerate() {
                assert_eq!(table.get(&(), name).unwrap(), Some(&i));
            }
            drop(table);
            drop(names);
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn scope_copies_retain_names_until_the_last_table_is_released() {
        let mut context = CallContext::new(CallOptions::default());
        let memory = Arc::downgrade(&context.identity());
        let work = Meter(RefCell::new(&mut context));
        let mut table = Table::new();
        table
            .insert(
                &work,
                Name::new(&work, &"long_name".repeat(512)).unwrap(),
                (),
            )
            .unwrap();
        let before = work.0.borrow().stats().retained_memory_bytes;
        let storage = table.buckets.len() * size_of::<Bucket<()>>();
        work.0.borrow_mut().options.limits.memory_bytes = Some(before + storage);
        let copy = table.copy(&work).unwrap();
        assert_eq!(
            work.0.borrow().stats().retained_memory_bytes,
            before + storage
        );
        drop(table);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, before);
        drop(context);
        assert!(memory.upgrade().is_some());
        assert!(copy.contains(&(), &"long_name".repeat(512)).unwrap());
        drop(copy);
        assert!(memory.upgrade().is_none());
    }

    #[test]
    fn deleted_buckets_compact_with_overlap_accounted_and_atomic_failure() {
        for shortage in [0, 1] {
            let mut context = CallContext::new(CallOptions::default());
            let work = Meter(RefCell::new(&mut context));
            let names: Vec<_> = [0, 1, 2, 3, 6]
                .map(|slot| {
                    let name = (0..)
                        .map(|n| format!("bucket_{n}"))
                        .find(|name| Table::<usize>::hash(&(), name).unwrap() & 7 == slot)
                        .unwrap();
                    Name::new(&work, &name).unwrap()
                })
                .into();
            let mut table = Table::new();
            for (i, name) in names.iter().take(4).enumerate() {
                table.insert(&work, name.clone(), i).unwrap();
            }
            for (i, name) in names.iter().take(3).enumerate() {
                assert_eq!(table.remove(&work, name).unwrap(), Some(i));
            }
            let before = work.0.borrow().stats().retained_memory_bytes;
            let storage = 8 * size_of::<Bucket<usize>>();
            work.0.borrow_mut().options.limits.memory_bytes = Some(before + storage - shortage);
            let result = table.insert(&work, names[4].clone(), 4);
            if shortage == 0 {
                assert_eq!(result.unwrap(), None);
                assert_eq!(table.deleted, 0);
                assert_eq!(table.get(&(), &names[4]).unwrap(), Some(&4));
                assert_eq!(work.0.borrow().stats().peak_memory_bytes, before + storage);
            } else {
                assert_eq!(result.unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(work.checkpoint().unwrap_err().kind, ErrorKind::Memory);
                assert_eq!(table.deleted, 3);
                assert_eq!(table.get(&(), &names[4]).unwrap(), None);
            }
            assert_eq!(table.buckets.len(), 8);
            assert_eq!(table.get(&(), &names[3]).unwrap(), Some(&3));
            for name in names.iter().take(3) {
                assert_eq!(table.get(&(), name).unwrap(), None);
            }
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, before);
            drop(table);
            drop(names);
            assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn table_churn_and_copies_match_an_independent_ordered_map() {
        let mut context = CallContext::new(CallOptions::default());
        let work = Meter(RefCell::new(&mut context));
        let mut names = names(&work, 64);
        names.extend(["", "\0", "日本語", "café"].map(|name| Name::new(&work, name).unwrap()));
        let mut table = Table::new();
        let mut reference = BTreeMap::new();
        let mut state = 7u64;
        for value in 0..1024 {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let name = &names[(state >> 32) as usize % names.len()];
            match state >> 60 {
                0..=7 => {
                    assert_eq!(
                        table.insert(&work, name.clone(), value).unwrap(),
                        reference.insert(name.as_str().to_owned(), value)
                    );
                }
                8..=11 => {
                    assert_eq!(
                        table.remove(&work, name).unwrap(),
                        reference.remove(name.as_str())
                    );
                }
                _ => assert_eq!(
                    table.get(&work, name).unwrap(),
                    reference.get(name.as_str())
                ),
            }
            if value % 31 == 0 {
                let copy = table.copy(&work).unwrap();
                let entries: BTreeMap<_, _> = copy
                    .iter(&work)
                    .unwrap()
                    .map(|(name, value)| (name.as_str().to_owned(), *value))
                    .collect();
                assert_eq!(entries, reference);
            }
        }
        drop(table);
        drop(names);
        assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
    }

    #[test]
    fn empty_tables_and_names_preserve_cancellation_and_deadlines() {
        for populated in [false, true] {
            for operation in ["lookup", "remove", "insert", "iterate", "copy"] {
                for deadline in [false, true] {
                    let mut table = Table::new();
                    if populated {
                        table.insert(&(), Name::default(), 7).unwrap();
                    }
                    let mut context = CallContext::new(CallOptions::default());
                    context.charge(1).unwrap();
                    let kind = if deadline {
                        context.options.deadline = Some(Instant::now());
                        ErrorKind::Deadline
                    } else {
                        context.cancellation().cancel();
                        ErrorKind::Cancelled
                    };
                    let work = Meter(RefCell::new(&mut context));
                    let result = match operation {
                        "lookup" => table.get(&work, "").map(drop),
                        "remove" => table.remove(&work, "").map(drop),
                        "insert" => table.insert(&work, Name::default(), 99).map(drop),
                        "iterate" => table.iter(&work).map(drop),
                        "copy" => table.copy(&work).map(drop),
                        _ => unreachable!(),
                    };
                    assert_eq!(result.unwrap_err().kind, kind);
                    assert_eq!(work.checkpoint().unwrap_err().kind, kind);
                    assert_eq!(table.get(&(), "").unwrap().copied(), populated.then_some(7));
                    assert_eq!(work.0.borrow().stats().retained_memory_bytes, 0);
                }
            }
        }
    }
}
