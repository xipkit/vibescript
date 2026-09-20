use super::facts::{Atom, Fact, Facts, Field, HashKind, Node, same_bytes};
use crate::{CallContext, Result, Value, budget::Buffer};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    value: Fact,
    depth: usize,
}

impl Key {
    fn bucket(self, mask: usize) -> usize {
        self.value
            .0
            .wrapping_mul(0x9e3779b1)
            .wrapping_add(self.depth.wrapping_mul(0x85ebca77))
            & mask
    }
}

struct Entry {
    key: Key,
    value: Fact,
    next: usize,
}

struct Memo {
    entries: Buffer<Entry>,
    buckets: Buffer<usize>,
}

impl Memo {
    fn new() -> Self {
        Self {
            entries: Buffer::empty(),
            buckets: Buffer::empty(),
        }
    }

    fn get(&self, ctx: &mut CallContext, key: Key) -> Result<Option<Fact>> {
        ctx.charge(1)?;
        if self.buckets.data.is_empty() {
            return Ok(None);
        }
        let mut index = self.buckets.data[key.bucket(self.buckets.data.len() - 1)];
        while index != usize::MAX {
            ctx.charge(1)?;
            let entry = &self.entries.data[index];
            if entry.key == key {
                return Ok(Some(entry.value));
            }
            index = entry.next;
        }
        Ok(None)
    }

    fn insert(&mut self, ctx: &mut CallContext, key: Key, value: Fact) -> Result<()> {
        if self.entries.data.len() >= self.buckets.data.len() / 2 {
            let Some(capacity) = self.buckets.data.len().max(8).checked_mul(2) else {
                return ctx.fail(
                    crate::ErrorKind::Memory,
                    "checker widening table size overflow",
                );
            };
            let mut buckets = Buffer::with_capacity(ctx, capacity)?;
            ctx.charge(capacity as u64 + self.entries.data.len() as u64)?;
            buckets.data.resize(capacity, usize::MAX);
            for (index, entry) in self.entries.data.iter_mut().enumerate() {
                let bucket = entry.key.bucket(capacity - 1);
                entry.next = buckets.data[bucket];
                buckets.data[bucket] = index;
            }
            self.buckets = buckets;
        }
        let bucket = key.bucket(self.buckets.data.len() - 1);
        let index = self.entries.data.len();
        self.entries.push(
            ctx,
            Entry {
                key,
                value,
                next: self.buckets.data[bucket],
            },
        )?;
        self.buckets.data[bucket] = index;
        Ok(())
    }
}

enum Task {
    Visit(Key),
    Save(Key),
    Literal(Fact),
    Union(Buffer<Fact>, usize),
    Array,
    Tuple(usize),
    Hash(Fact, HashKind),
    Shape(Buffer<Field>, bool, Fact, HashKind),
}

impl Facts {
    pub(super) fn joined(
        &mut self,
        ctx: &mut CallContext,
        a: Fact,
        b: Fact,
        depth: Option<usize>,
    ) -> Result<Fact> {
        match depth {
            Some(depth) => self.widen(ctx, a, b, depth),
            None => self.union(ctx, &[a, b]),
        }
    }

    pub fn widen(&mut self, ctx: &mut CallContext, a: Fact, b: Fact, depth: usize) -> Result<Fact> {
        ctx.checkpoint()?;
        if a == b {
            return Ok(a);
        }
        if let (Some(a), Some(b)) = (self.integer_hull(ctx, a)?, self.integer_hull(ctx, b)?) {
            return self.integer_range(ctx, a.widen(b));
        }
        let value = self.union(ctx, &[a, b])?;
        let mut tasks = Buffer::empty();
        let mut values = Buffer::empty();
        let mut memo = Memo::new();
        tasks.push(ctx, Task::Visit(Key { value, depth }))?;
        while let Some(task) = tasks.data.pop() {
            ctx.charge(1)?;
            let value = match task {
                Task::Literal(value) => value,
                Task::Save(key) => {
                    memo.insert(ctx, key, *values.data.last().unwrap())?;
                    continue;
                }
                Task::Array => self.array(ctx, values.data.pop().unwrap())?,
                Task::Tuple(count) => {
                    let start = values.data.len() - count;
                    let value = self.tuple(ctx, &values.data[start..])?;
                    values.data.truncate(start);
                    value
                }
                Task::Hash(keys, plain) => {
                    self.hash_kind(ctx, keys, values.data.pop().unwrap(), plain)?
                }
                Task::Shape(mut fields, open, keys, plain) => {
                    let start = values.data.len() - fields.data.len();
                    for (field, &value) in fields.data.iter_mut().zip(&values.data[start..]) {
                        ctx.charge(1)?;
                        field.value = value;
                    }
                    values.data.truncate(start);
                    self.shape_fields(ctx, fields, open, keys, plain)?
                }
                Task::Union(mut arms, count) => {
                    let start = values.data.len() - count;
                    arms.extend(ctx, &values.data[start..])?;
                    values.data.truncate(start);
                    self.union(ctx, &arms.data)?
                }
                Task::Visit(key) => {
                    if let Some(value) = memo.get(ctx, key)? {
                        values.push(ctx, value)?;
                        continue;
                    }
                    if self.arm_count(key.value) == 1 && self.depth(key.value) <= key.depth {
                        values.push(ctx, key.value)?;
                        continue;
                    }
                    let mut scalar = Buffer::empty();
                    let mut arrays = Buffer::empty();
                    let mut hashes = Buffer::empty();
                    for i in 0..self.arm_count(key.value) {
                        ctx.charge(1)?;
                        let arm = self.arm(key.value, i);
                        match self.node(arm) {
                            Node::Array(_) | Node::Tuple(_) => arrays.push(ctx, arm)?,
                            Node::Hash(..) | Node::Shape(..) => hashes.push(ctx, arm)?,
                            _ => scalar.push(ctx, arm)?,
                        }
                    }
                    let mut count = 0;
                    let mut bounds: Option<super::integers::Bounds> = None;
                    let mut previous = None;
                    for &value in &scalar.data {
                        ctx.charge(1)?;
                        if let Some(next) = self.integer_bounds(value) {
                            count += 1;
                            bounds = Some(bounds.map_or(next, |before| before.hull(next)));
                            if previous.is_none()
                                && matches!(self.node(value), Node::IntegerBounds(_))
                            {
                                previous = Some(next);
                            }
                        }
                    }
                    if count > 1 {
                        let bounds = bounds.unwrap();
                        let bounds = previous.map_or(bounds, |before| before.widen(bounds));
                        ctx.charge(scalar.data.len() as u64)?;
                        scalar
                            .data
                            .retain(|&value| self.integer_bounds(value).is_none());
                        let value = self.integer_range(ctx, bounds)?;
                        scalar.push(ctx, value)?;
                    }
                    let count =
                        usize::from(!arrays.data.is_empty()) + usize::from(!hashes.data.is_empty());
                    tasks.push(ctx, Task::Save(key))?;
                    tasks.push(ctx, Task::Union(scalar, count))?;
                    if !hashes.data.is_empty() {
                        self.widen_hashes(ctx, &mut tasks, &hashes.data, key.depth)?;
                    }
                    if !arrays.data.is_empty() {
                        self.widen_arrays(ctx, &mut tasks, &arrays.data, key.depth)?;
                    }
                    continue;
                }
            };
            values.push(ctx, value)?;
        }
        assert_eq!(values.data.len(), 1);
        Ok(values.data[0])
    }

    fn widen_arrays(
        &mut self,
        ctx: &mut CallContext,
        tasks: &mut Buffer<Task>,
        arrays: &[Fact],
        depth: usize,
    ) -> Result<()> {
        if depth == 0 {
            let value = self.array(ctx, Atom::Unknown.fact())?;
            return tasks.push(ctx, Task::Literal(value));
        }
        let mut length = match self.node(arrays[0]) {
            Node::Tuple(elements) => Some(elements.data.len()),
            _ => None,
        };
        for &array in arrays {
            ctx.charge(1)?;
            if !matches!(self.node(array), Node::Tuple(elements) if Some(elements.data.len()) == length)
            {
                length = None;
            }
        }
        if let Some(length) = length {
            tasks.push(ctx, Task::Tuple(length))?;
            for index in (0..length).rev() {
                let mut children = Buffer::empty();
                for &array in arrays {
                    ctx.charge(1)?;
                    let Node::Tuple(elements) = self.node(array) else {
                        unreachable!()
                    };
                    children.push(ctx, elements.data[index])?;
                }
                let value = self.union(ctx, &children.data)?;
                tasks.push(
                    ctx,
                    Task::Visit(Key {
                        value,
                        depth: depth - 1,
                    }),
                )?;
            }
        } else {
            let mut children = Buffer::empty();
            for &array in arrays {
                ctx.charge(1)?;
                match self.node(array) {
                    Node::Array(element) => children.push(ctx, *element)?,
                    Node::Tuple(elements) => children.extend(ctx, &elements.data)?,
                    _ => unreachable!(),
                }
            }
            let value = self.union(ctx, &children.data)?;
            tasks.push(ctx, Task::Array)?;
            tasks.push(
                ctx,
                Task::Visit(Key {
                    value,
                    depth: depth - 1,
                }),
            )?;
        }
        Ok(())
    }

    fn widen_hashes(
        &mut self,
        ctx: &mut CallContext,
        tasks: &mut Buffer<Task>,
        hashes: &[Fact],
        depth: usize,
    ) -> Result<()> {
        let (mut plain, mut shapes, mut open) = (None, true, false);
        let mut keys = Buffer::empty();
        let mut elements = Buffer::empty();
        for &hash in hashes {
            ctx.charge(1)?;
            let kind = self.hash_mode(hash);
            plain = Some(plain.map_or(kind, |previous: HashKind| previous.join(kind)));
            match self.node(hash) {
                Node::Hash(key, element, _) => {
                    shapes = false;
                    keys.push(ctx, *key)?;
                    elements.push(ctx, *element)?;
                }
                Node::Shape(fields, any, key, _) => {
                    open |= any;
                    keys.push(ctx, *key)?;
                    if *any {
                        elements.push(ctx, Atom::Unknown.fact())?;
                    }
                    for field in &fields.data {
                        elements.push(ctx, field.value)?;
                    }
                }
                _ => unreachable!(),
            }
        }
        let plain = plain.unwrap_or(HashKind::ANY);
        let keys = self.union(ctx, &keys.data)?;
        if depth == 0 {
            let value = self.hash_kind(ctx, keys, Atom::Unknown.fact(), plain)?;
            return tasks.push(ctx, Task::Literal(value));
        }
        if !shapes {
            let value = self.union(ctx, &elements.data)?;
            tasks.push(ctx, Task::Hash(keys, plain))?;
            return tasks.push(
                ctx,
                Task::Visit(Key {
                    value,
                    depth: depth - 1,
                }),
            );
        }
        let mut source = Buffer::empty();
        let mut work = 0usize;
        for &hash in hashes {
            let Node::Shape(fields, ..) = self.node(hash) else {
                unreachable!()
            };
            for field in &fields.data {
                ctx.charge(1)?;
                work = work.saturating_add(field.name.as_bytes().unwrap().len().saturating_add(1));
                source.push(ctx, (field.name.clone(), field.value, field.optional))?;
            }
        }
        ctx.charge(work.saturating_mul(source.data.len().max(1).ilog2() as usize + 1) as u64)?;
        source
            .data
            .sort_unstable_by(|a, b| a.0.as_bytes().cmp(&b.0.as_bytes()));
        let mut fields = Buffer::empty();
        let mut current: Option<(Value, Fact, bool, usize)> = None;
        for (name, value, optional) in source.data {
            ctx.charge(1)?;
            if let Some((key, into, maybe, count)) = &mut current {
                if same_bytes(ctx, key, &name)? {
                    *into = self.union(ctx, &[*into, value])?;
                    *maybe |= optional;
                    *count += 1;
                    continue;
                }
            }
            if let Some(field) = current.take() {
                self.widen_field(ctx, &mut fields, field, hashes.len(), open)?;
            }
            current = Some((name, value, optional, 1));
        }
        if let Some(field) = current {
            self.widen_field(ctx, &mut fields, field, hashes.len(), open)?;
        }
        let mut children = Buffer::empty();
        for field in &fields.data {
            children.push(ctx, field.value)?;
        }
        tasks.push(ctx, Task::Shape(fields, open, keys, plain))?;
        for &value in children.data.iter().rev() {
            tasks.push(
                ctx,
                Task::Visit(Key {
                    value,
                    depth: depth - 1,
                }),
            )?;
        }
        Ok(())
    }

    fn widen_field(
        &mut self,
        ctx: &mut CallContext,
        fields: &mut Buffer<Field>,
        field: (Value, Fact, bool, usize),
        count: usize,
        open: bool,
    ) -> Result<()> {
        let (name, mut value, optional, present) = field;
        if open && present != count {
            value = self.union(ctx, &[value, Atom::Unknown.fact()])?;
        }
        fields.push(
            ctx,
            Field {
                name,
                value,
                optional: optional || present != count,
            },
        )
    }
}
