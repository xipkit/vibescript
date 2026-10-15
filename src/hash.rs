use crate::{
    CallContext, ErrorKind, Result, Value,
    budget::{Buffer, CHUNK, Charge, MAX_VALUE_DEPTH},
    json::bytes_equal,
};
use std::{collections::hash_map::DefaultHasher, hash::Hasher, mem::size_of, sync::Arc};

const INDEX_THRESHOLD: usize = 16;
const EMPTY: usize = usize::MAX;

#[derive(Debug)]
struct Index {
    buckets: Buffer<usize>,
    _header: Option<Charge>,
}

impl Index {
    fn new(ctx: &mut CallContext, size: usize) -> Result<Arc<Self>> {
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        let mut buckets = Buffer::with_capacity(ctx, size)?;
        while buckets.data.len() < size {
            let end = size.min(buckets.data.len() + CHUNK / size_of::<usize>());
            ctx.work_bytes((end - buckets.data.len()) * size_of::<usize>())?;
            buckets.data.resize(end, EMPTY);
        }
        Ok(Arc::new(Self {
            buckets,
            _header: header,
        }))
    }

    fn make_mut<'a>(ctx: &mut CallContext, index: &'a mut Arc<Self>) -> Result<&'a mut Self> {
        if Arc::get_mut(index).is_none() {
            let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
            let mut buckets = Buffer::with_capacity(ctx, index.buckets.data.len())?;
            buckets.extend(ctx, &index.buckets.data)?;
            *index = Arc::new(Self {
                buckets,
                _header: header,
            });
        }
        Ok(Arc::get_mut(index).unwrap())
    }

    fn vacant(&self, ctx: &mut CallContext, hash: usize) -> Result<usize> {
        let mask = self.buckets.data.len() - 1;
        let mut slot = hash & mask;
        loop {
            ctx.charge(1)?;
            if self.buckets.data[slot] == EMPTY {
                return Ok(slot);
            }
            slot = (slot + 1) & mask;
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tag {
    #[default]
    None,
    Match,
    Error,
}
impl Tag {
    pub fn protected(self) -> bool {
        self != Self::None
    }
    /// Rejects `operation` on a protected record, naming the operation first.
    pub fn mutation_error(self, operation: &str) -> crate::Error {
        crate::Error::new(
            ErrorKind::Argument,
            if self == Self::Error {
                format!("{operation} cannot modify a rescued error")
            } else {
                format!("{operation} cannot modify match data")
            },
        )
    }
}

/// A key to store: a string value, or a name to copy into one only when
/// the entry is new.
enum Key<'a> {
    Value(Value),
    Name(&'a [u8]),
}

impl Key<'_> {
    fn bytes(&self) -> Result<&[u8]> {
        match self {
            Self::Value(key) => key.require_bytes(),
            Self::Name(name) => Ok(name),
        }
    }
}

#[derive(Debug)]
pub(crate) struct Hash {
    pub buffer: Buffer<(Value, Value)>,
    index: Option<Arc<Index>>,
    pub header: Option<Charge>,
    pub depth: usize,
    /// How many values sit at `depth - 1`, so overwriting one of them with
    /// a shallower value only rescans for a new maximum when the last one
    /// leaves.
    pub(crate) at_max: usize,
    pub object: bool,
    pub tag: Tag,
    pub(crate) drop_parent: Option<Value>,
}

impl Drop for Hash {
    fn drop(&mut self) {
        // Tall hashes hand their entries to iterative destruction so that a
        // chain of nested containers never unwinds through depth-proportional glue.
        if self.depth > crate::value::destroy::SHALLOW && !self.buffer.data.is_empty() {
            crate::value::destroy::pairs(std::mem::replace(&mut self.buffer, Buffer::empty()));
        }
    }
}

impl Hash {
    pub fn empty() -> Self {
        Self {
            buffer: Buffer::empty(),
            index: None,
            header: None,
            depth: 1,
            at_max: 0,
            object: false,
            tag: Tag::None,
            drop_parent: None,
        }
    }

    pub fn untracked(data: Vec<(Value, Value)>, depth: usize) -> Arc<Self> {
        let at_max = data
            .iter()
            .filter(|(_, v)| v.depth() + 1 == depth && depth > 1)
            .count();
        let mut hash = Self::empty();
        hash.buffer = Buffer::untracked(data);
        hash.depth = depth;
        hash.at_max = at_max;
        Arc::new(hash)
    }

    pub(crate) fn into_buffer(mut self) -> Buffer<(Value, Value)> {
        std::mem::replace(&mut self.buffer, Buffer::empty())
    }

    pub fn from_entries(ctx: &mut CallContext, buffer: Buffer<(Value, Value)>) -> Result<Self> {
        let mut hash = Self::empty();
        hash.buffer = buffer;
        let (depth, at_max) = values_depth(ctx, &hash.buffer.data, None)?;
        hash.depth = depth;
        hash.at_max = at_max;
        hash.check_depth(ctx)?;
        hash.ensure_index(ctx, hash.buffer.data.len())?;
        Ok(hash)
    }

    pub fn into_arc(mut self, ctx: &mut CallContext) -> Result<Arc<Self>> {
        self.check_depth(ctx)?;
        self.header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(self))
    }

    pub fn make_mut<'a>(ctx: &mut CallContext, hash: &'a mut Arc<Self>) -> Result<&'a mut Self> {
        if hash.tag.protected() {
            return Err(hash.tag.mutation_error("assignment"));
        }
        if Arc::get_mut(hash).is_none() {
            let mut buffer = Buffer::with_capacity(ctx, hash.buffer.data.len())?;
            buffer.extend(ctx, &hash.buffer.data)?;
            *hash = Self {
                buffer,
                index: hash.index.clone(),
                header: None,
                depth: hash.depth,
                at_max: hash.at_max,
                object: hash.object,
                tag: hash.tag,
                drop_parent: None,
            }
            .into_arc(ctx)?;
        }
        Ok(Arc::get_mut(hash).unwrap())
    }

    /// Records a container's nesting depth and the count of entries at that
    /// maximum, as a builder that tracks them itself stores them.
    pub(crate) fn set_depth_count(&mut self, depth: usize, at_max: usize) {
        self.depth = depth;
        self.at_max = at_max;
    }

    pub fn find(&self, ctx: &mut CallContext, key: &[u8]) -> Result<Option<usize>> {
        let hash = if self.index.is_some() {
            hash_key(ctx, key)?
        } else {
            0
        };
        self.find_hashed(ctx, key, hash)
    }

    fn find_hashed(&self, ctx: &mut CallContext, key: &[u8], hash: usize) -> Result<Option<usize>> {
        if let Some(index) = &self.index {
            let mask = index.buckets.data.len() - 1;
            let mut slot = hash & mask;
            loop {
                ctx.charge(1)?;
                let entry = index.buckets.data[slot];
                if entry == EMPTY {
                    return Ok(None);
                }
                if bytes_equal(ctx, self.buffer.data[entry].0.require_bytes()?, key)? {
                    return Ok(Some(entry));
                }
                slot = (slot + 1) & mask;
            }
        }
        for (i, (candidate, _)) in self.buffer.data.iter().enumerate() {
            ctx.charge(1)?;
            if bytes_equal(ctx, candidate.require_bytes()?, key)? {
                return Ok(Some(i));
            }
        }
        Ok(None)
    }

    pub fn insert(&mut self, ctx: &mut CallContext, key: Value, value: Value) -> Result<()> {
        self.insert_with_limit(ctx, Key::Value(key), value, MAX_VALUE_DEPTH)
    }

    /// Stores a field without counting the internal field table as a value container.
    pub fn insert_field(&mut self, ctx: &mut CallContext, key: Value, value: Value) -> Result<()> {
        self.insert_with_limit(ctx, Key::Value(key), value, MAX_VALUE_DEPTH + 1)
    }

    /// Stores a field by `name`, as [`Self::insert_field`] does, copying the
    /// name into a key only when the field is new.
    pub fn insert_named_field(
        &mut self,
        ctx: &mut CallContext,
        name: &[u8],
        value: Value,
    ) -> Result<()> {
        self.insert_with_limit(ctx, Key::Name(name), value, MAX_VALUE_DEPTH + 1)
    }

    fn insert_with_limit(
        &mut self,
        ctx: &mut CallContext,
        key: Key<'_>,
        value: Value,
        limit: usize,
    ) -> Result<()> {
        ctx.charge(1)?;
        let hash = if self.index.is_some() || self.buffer.data.len() >= INDEX_THRESHOLD - 1 {
            hash_key(ctx, key.bytes()?)?
        } else {
            0
        };
        let existing = self.find_hashed(ctx, key.bytes()?, hash)?;
        let old_depth = existing
            .map(|i| self.buffer.data[i].1.depth() + 1)
            .unwrap_or(0);
        let new_depth = value.depth() + 1;
        let mut depth = self.depth.max(new_depth);
        let mut at_max = self.at_max;
        if new_depth > self.depth {
            // The new value is deeper than any current maximum, so it is the
            // only one at the new maximum.
            at_max = 1;
        } else if existing.is_some()
            && old_depth == self.depth
            && self.depth > 1
            && new_depth < self.depth
        {
            // The overwritten value sat at the maximum and the new one is
            // shallower. Only walk for a new maximum when it was the last
            // one; otherwise the count alone says the maximum stands.
            if at_max > 1 {
                at_max -= 1;
            } else {
                let (walked, count) = values_depth(ctx, &self.buffer.data, existing)?;
                depth = walked.max(new_depth);
                at_max = if depth == walked {
                    count + usize::from(new_depth == walked && walked > 1)
                } else {
                    1
                };
            }
        } else if new_depth == depth && depth > 1 && old_depth != depth {
            // The new value joins the current maximum without replacing one
            // there.
            at_max += 1;
        }
        if depth > limit {
            return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
        }
        if let Some(i) = existing {
            self.buffer.data[i].1 = value;
        } else {
            let Some(len) = self.buffer.data.len().checked_add(1) else {
                return ctx.fail(ErrorKind::Memory, "hash size overflow");
            };
            let key = match key {
                Key::Value(key) => key,
                Key::Name(name) => ctx.bytes(name)?,
            };
            self.ensure_index(ctx, len)?;
            let slot = self
                .index
                .as_ref()
                .map(|index| index.vacant(ctx, hash))
                .transpose()?;
            self.buffer.push(ctx, (key, value))?;
            if let Some(slot) = slot {
                // ensure_index made the table unique before entries were changed.
                let index = Arc::get_mut(self.index.as_mut().unwrap()).unwrap();
                index.buckets.data[slot] = len - 1;
            }
        }
        self.depth = depth;
        self.at_max = at_max;
        Ok(())
    }

    /// Removes the entries at the distinct positions in `indices` in one
    /// linear pass, charging one step per entry the compaction visits and
    /// for marking the deletion bitmap, then shrinking, recomputing depth
    /// and rebuilding the index once. A batch costs the same finds and
    /// per-entry work as repeated [`Self::remove`] calls without shifting
    /// the buffer and rebuilding the whole index after every key, which
    /// makes a bulk `delete_if` quadratic in the hash size. Positions need
    /// no ordering: a bitmap marks them, so the pass stays linear whatever
    /// order the block's mutations leave them in. The position buffer is
    /// consumed, so its reservation is released with the bitmap before the
    /// replacement allocations begin.
    pub fn remove_keys(&mut self, ctx: &mut CallContext, indices: Buffer<usize>) -> Result<()> {
        self.index = None;
        let len = self.buffer.data.len();
        // A byte mark flags each removed position without ordering them. The
        // marks are allocated through the fallible `Buffer` path so allocator
        // exhaustion surfaces as the invocation's memory error, their bytes
        // are reserved against the quota, and their zeroing is charged per 64
        // bytes, as the index fill charges per bucket word, so the allocation
        // cannot bypass the step quota. (`Vec<bool>` would pack eight marks
        // per byte, so a byte per mark keeps the reservation and the charge
        // honest.)
        let mut marked = Buffer::with_capacity(ctx, len)?;
        while marked.data.len() < len {
            let end = len.min(marked.data.len() + CHUNK);
            ctx.work_bytes(end - marked.data.len())?;
            marked.data.resize(end, 0);
        }
        // Mark in bounded groups, resting at a checkpoint between each, so a
        // cancellation or deadline raised while a large batch is being marked
        // stops it instead of waiting for the compaction below to notice.
        for slots in indices.data.chunks(CHUNK) {
            for slot in slots {
                if *slot < len {
                    marked.data[*slot] = 1;
                }
            }
            ctx.checkpoint()?;
        }
        // The positions are spent; release their reservation before the
        // compaction so a deletion near the quota does not carry the dead
        // buffer alongside the marks.
        drop(indices);
        let mut write = 0;
        let mut depth = 1;
        let mut at_max = 0;
        let mut deepest_removed = false;
        for read in 0..len {
            ctx.charge(1)?;
            if marked.data[read] != 0 {
                if self.buffer.data[read].1.depth() + 1 == self.depth {
                    deepest_removed = true;
                }
                continue;
            }
            if write != read {
                self.buffer.data.swap(write, read);
            }
            let child = self.buffer.data[write].1.depth() + 1;
            if child > depth {
                depth = child;
                at_max = 1;
            } else if child == depth && depth > 1 {
                at_max += 1;
            }
            write += 1;
        }
        // Discard the removed entries in bounded groups with a checkpoint
        // between each, so destroying them observes a cancellation raised
        // after the compaction instead of dropping them all unwatched.
        let mut removed = len - write;
        while removed > 0 {
            let step = removed.min(CHUNK);
            ctx.work_bytes(step)?;
            ctx.checkpoint()?;
            for _ in 0..step {
                self.buffer.data.pop();
            }
            removed -= step;
        }
        if deepest_removed {
            self.depth = depth;
            self.at_max = at_max;
        }
        // The bitmap's work is done; release its reservation before the
        // replacement allocations so a deletion near the quota does not need
        // the headroom for both at once.
        drop(marked);
        if self.buffer.data.is_empty() {
            self.buffer = Buffer::empty();
        } else if self.buffer.data.len() < self.buffer.data.capacity() / 2 {
            let mut buffer = Buffer::with_capacity(ctx, self.buffer.data.len())?;
            buffer.extend(ctx, &self.buffer.data)?;
            self.buffer = buffer;
        }
        self.ensure_index(ctx, self.buffer.data.len())?;
        Ok(())
    }

    pub fn remove(&mut self, ctx: &mut CallContext, index: usize) -> Result<Value> {
        self.index = None;
        for i in index..self.buffer.data.len() - 1 {
            ctx.charge(1)?;
            self.buffer.data.swap(i, i + 1);
        }
        let (_, removed) = self.buffer.data.pop().unwrap();
        if self.buffer.data.is_empty() {
            self.buffer = Buffer::empty();
        } else if self.buffer.data.len() < self.buffer.data.capacity() / 2 {
            let mut buffer = Buffer::with_capacity(ctx, self.buffer.data.len())?;
            buffer.extend(ctx, &self.buffer.data)?;
            self.buffer = buffer;
        }
        if removed.depth() + 1 == self.depth {
            let (depth, at_max) = values_depth(ctx, &self.buffer.data, None)?;
            self.depth = depth;
            self.at_max = at_max;
        }
        self.ensure_index(ctx, self.buffer.data.len())?;
        Ok(removed)
    }

    fn check_depth(&self, ctx: &mut CallContext) -> Result<()> {
        if self.depth > MAX_VALUE_DEPTH {
            return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
        }
        Ok(())
    }

    fn ensure_index(&mut self, ctx: &mut CallContext, len: usize) -> Result<()> {
        if len < INDEX_THRESHOLD {
            return Ok(());
        }
        if let Some(index) = &mut self.index {
            if len <= index.buckets.data.len() / 4 * 3 {
                Index::make_mut(ctx, index)?;
                return Ok(());
            }
        }
        let Some(size) = len
            .checked_mul(2)
            .and_then(usize::checked_next_power_of_two)
        else {
            return ctx.fail(ErrorKind::Memory, "hash index size overflow");
        };
        let mut index = Index::new(ctx, size)?;
        let writable = Arc::get_mut(&mut index).unwrap();
        for (i, (key, _)) in self.buffer.data.iter().enumerate() {
            let hash = hash_key(ctx, key.require_bytes()?)?;
            let slot = writable.vacant(ctx, hash)?;
            writable.buckets.data[slot] = i;
        }
        self.index = Some(index);
        Ok(())
    }
}

fn hash_key(ctx: &mut CallContext, key: &[u8]) -> Result<usize> {
    // Deterministic hashing keeps work counters reproducible. Every collision probe and
    // byte comparison is charged, so colliding keys cannot bypass execution limits.
    let mut hasher = DefaultHasher::new();
    for chunk in key.chunks(CHUNK) {
        ctx.work_bytes(chunk.len())?;
        hasher.write(chunk);
    }
    Ok(hasher.finish() as usize)
}

/// Returns a hash's value nesting depth and how many values sit at that
/// maximum, charging one step per entry, as the walks it replaces did.
/// `except` excludes an entry being replaced, whose old value must not
/// count toward the maximum.
fn values_depth(
    ctx: &mut CallContext,
    entries: &[(Value, Value)],
    except: Option<usize>,
) -> Result<(usize, usize)> {
    let mut depth = 1;
    let mut at_max = 0;
    for (i, (_, value)) in entries.iter().enumerate() {
        ctx.charge(1)?;
        if Some(i) == except {
            continue;
        }
        let child = value.depth() + 1;
        if child > depth {
            depth = child;
            at_max = 1;
        } else if child == depth && depth > 1 {
            at_max += 1;
        }
    }
    Ok((depth, at_max))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn collisions_preserve_keys_and_consume_work() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut keys = Vec::new();
        for n in 0..100_000 {
            let key = format!("collision-{n}");
            let mut hasher = DefaultHasher::new();
            hasher.write(key.as_bytes());
            if hasher.finish() & 511 == 0 {
                keys.push(key);
                if keys.len() == 101 {
                    break;
                }
            }
        }
        assert_eq!(keys.len(), 101);
        let mut hash = Hash::empty();
        for (i, key) in keys[..100].iter().enumerate() {
            let key = ctx.bytes(key.as_bytes()).unwrap();
            hash.insert(&mut ctx, key, Value::int(i as i64)).unwrap();
        }
        for (i, key) in keys[..100].iter().enumerate() {
            assert_eq!(hash.find(&mut ctx, key.as_bytes()).unwrap(), Some(i));
        }
        assert_eq!(hash.find(&mut ctx, keys[100].as_bytes()).unwrap(), None);

        for i in (0..100).step_by(2) {
            let index = hash.find(&mut ctx, keys[i].as_bytes()).unwrap().unwrap();
            assert_eq!(
                hash.remove(&mut ctx, index).unwrap().as_int(),
                Some(i as i64)
            );
        }
        for (i, key) in keys[..100].iter().enumerate() {
            let expected = (i % 2 == 1).then_some(i / 2);
            assert_eq!(hash.find(&mut ctx, key.as_bytes()).unwrap(), expected);
        }

        let used = ctx.stats().steps;
        ctx.options.limits.steps = Some(used + 32);
        assert_eq!(
            hash.find(&mut ctx, keys[100].as_bytes()).unwrap_err().kind,
            ErrorKind::Steps
        );
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
        drop(hash);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn batched_removal_compacts_once_and_rebuilds_the_index_once() {
        let batched = |keys: &[&str]| -> (u64, Hash) {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut hash = Hash::empty();
            for (i, key) in keys.iter().enumerate() {
                let key = ctx.bytes(key.as_bytes()).unwrap();
                hash.insert(&mut ctx, key, Value::int(i as i64)).unwrap();
            }
            let indices: Vec<usize> = keys
                .iter()
                .enumerate()
                .filter(|(i, _)| i % 2 == 0)
                .map(|(i, _)| i)
                .collect();
            let before = ctx.stats().steps;
            hash.remove_keys(&mut ctx, Buffer::untracked(indices))
                .unwrap();
            (ctx.stats().steps - before, hash)
        };
        let singles = |keys: &[&str]| -> (u64, Hash) {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut hash = Hash::empty();
            for (i, key) in keys.iter().enumerate() {
                let key = ctx.bytes(key.as_bytes()).unwrap();
                hash.insert(&mut ctx, key, Value::int(i as i64)).unwrap();
            }
            let before = ctx.stats().steps;
            for (i, key) in keys.iter().enumerate().filter(|(i, _)| i % 2 == 0) {
                let index = hash.find(&mut ctx, key.as_bytes()).unwrap().unwrap();
                let removed = hash.remove(&mut ctx, index).unwrap();
                assert_eq!(removed.as_int(), Some(i as i64));
            }
            (ctx.stats().steps - before, hash)
        };
        let keys: Vec<String> = (0..40).map(|i| format!("k{i}")).collect();
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let (batched_steps, batched_hash) = batched(&keys);
        let (single_steps, single_hash) = singles(&keys);
        // The batch must charge strictly less than one-at-a-time removal,
        // which shifts the buffer and rebuilds the whole index per key.
        assert!(
            batched_steps < single_steps,
            "{batched_steps} !< {single_steps}"
        );
        let mut ctx = CallContext::new(CallOptions::default());
        // Both orders keep exactly the odd keys, in order, with a working index.
        assert_eq!(batched_hash.buffer.data.len(), 20);
        assert_eq!(single_hash.buffer.data.len(), 20);
        for (i, key) in keys.iter().enumerate() {
            let expected = if i % 2 == 1 { Some(i / 2) } else { None };
            assert_eq!(
                batched_hash.find(&mut ctx, key.as_bytes()).unwrap(),
                expected
            );
        }
        for (i, key) in keys.iter().enumerate() {
            let expected = if i % 2 == 1 { Some(i / 2) } else { None };
            assert_eq!(
                single_hash.find(&mut ctx, key.as_bytes()).unwrap(),
                expected
            );
        }
        // The rebuilt index stays correct for new inserts.
        let key = ctx.bytes(b"new").unwrap();
        let mut hash = batched_hash;
        hash.insert(&mut ctx, key, Value::int(40)).unwrap();
        assert_eq!(hash.find(&mut ctx, b"new").unwrap(), Some(20));
        assert_eq!(hash.buffer.data.len(), 21);
    }

    #[test]
    fn batched_removal_recovers_depth_when_the_deepest_entries_leave() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut hash = Hash::empty();
        for i in 0..20 {
            let key = ctx.bytes(format!("k{i}").as_bytes()).unwrap();
            hash.insert(&mut ctx, key, Value::int(i)).unwrap();
        }
        let mut nested = Value::int(0);
        for _ in 0..64 {
            nested = Value::array(vec![nested]);
        }
        let key = ctx.bytes(b"deep").unwrap();
        hash.insert(&mut ctx, key, nested).unwrap();
        assert!(hash.depth > 64);
        // Removing every scalar entry keeps the deep one, so the depth stays.
        let scalars: Vec<usize> = (0..20).collect();
        hash.remove_keys(&mut ctx, Buffer::untracked(scalars))
            .unwrap();
        assert!(hash.depth > 64);
        // Removing the deep entry recovers a depth of one for the empty hash.
        let deep = hash.find(&mut ctx, b"deep").unwrap().unwrap();
        hash.remove_keys(&mut ctx, Buffer::untracked(vec![deep]))
            .unwrap();
        assert_eq!(hash.depth, 1);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn index_capacity_is_reserved_before_allocation() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut hash = Hash::empty();
        for i in 0..15 {
            let key = ctx.bytes(format!("k{i}").as_bytes()).unwrap();
            hash.insert(&mut ctx, key, Value::int(i)).unwrap();
        }
        let key = ctx.bytes(b"new").unwrap();
        let used = ctx.stats().retained_memory_bytes;
        let index_bytes = size_of::<Index>() + 2 * size_of::<usize>() + 32 * size_of::<usize>();
        ctx.options.limits.memory_bytes = Some(used + index_bytes - 1);
        assert_eq!(
            hash.insert(&mut ctx, key.clone(), Value::nil())
                .unwrap_err()
                .kind,
            ErrorKind::Memory
        );
        assert_eq!(hash.buffer.data.len(), 15);
        assert!(hash.index.is_none());
        assert_eq!(ctx.stats().retained_memory_bytes, used);
        drop(key);
        drop(hash);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn long_keys_observe_work_limits_and_cancellation() {
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = Some(128);
        assert_eq!(
            hash_key(&mut ctx, &[b'x'; CHUNK * 8]).unwrap_err().kind,
            ErrorKind::Steps
        );
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.cancellation().cancel();
        assert_eq!(
            hash_key(&mut ctx, &[b'x'; CHUNK * 8]).unwrap_err().kind,
            ErrorKind::Cancelled
        );
    }

    #[test]
    fn imported_indexes_have_independent_lifetimes() {
        let input = Value::hash(
            (0..128)
                .map(|i| (format!("k{i}").into_bytes(), Value::int(i)))
                .collect(),
        );
        let mut first = CallContext::new(CallOptions::default());
        let mut second = CallContext::new(CallOptions::default());
        let a = first.import(&input).unwrap();
        let b = second.import(&a).unwrap();
        let used = second.stats().retained_memory_bytes;
        let alias = second.import(&b).unwrap();
        assert_eq!(second.stats().retained_memory_bytes, used);
        drop(a);
        assert_eq!(first.stats().retained_memory_bytes, 0);
        assert_eq!(second.stats().retained_memory_bytes, used);
        drop(b);
        assert_eq!(second.stats().retained_memory_bytes, used);
        drop(alias);
        assert_eq!(second.stats().retained_memory_bytes, 0);
    }
}
