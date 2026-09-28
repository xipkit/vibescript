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
            object: false,
            tag: Tag::None,
            drop_parent: None,
        }
    }

    pub fn untracked(data: Vec<(Value, Value)>, depth: usize) -> Arc<Self> {
        let mut hash = Self::empty();
        hash.buffer = Buffer::untracked(data);
        hash.depth = depth;
        Arc::new(hash)
    }

    pub(crate) fn into_buffer(mut self) -> Buffer<(Value, Value)> {
        std::mem::replace(&mut self.buffer, Buffer::empty())
    }

    pub fn from_entries(ctx: &mut CallContext, buffer: Buffer<(Value, Value)>) -> Result<Self> {
        let mut hash = Self::empty();
        hash.buffer = buffer;
        for (_, value) in &hash.buffer.data {
            ctx.charge(1)?;
            hash.depth = hash.depth.max(value.depth() + 1);
        }
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
                object: hash.object,
                tag: hash.tag,
                drop_parent: None,
            }
            .into_arc(ctx)?;
        }
        Ok(Arc::get_mut(hash).unwrap())
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
        let mut depth = self.depth.max(value.depth() + 1);
        if let Some(i) = existing {
            if self.depth > 1
                && self.buffer.data[i].1.depth() + 1 == self.depth
                && value.depth() + 1 < self.depth
            {
                depth = value.depth() + 1;
                for (j, (_, v)) in self.buffer.data.iter().enumerate() {
                    ctx.charge(1)?;
                    if i != j {
                        depth = depth.max(v.depth() + 1);
                    }
                }
            }
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
            self.depth = 1;
            for (_, value) in &self.buffer.data {
                ctx.charge(1)?;
                self.depth = self.depth.max(value.depth() + 1);
            }
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
