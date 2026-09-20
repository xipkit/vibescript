use super::facts::{Atom, Fact, Facts, Field, HashKind};
use crate::{
    CallContext, ErrorKind, Result, Value,
    budget::{Buffer, MAX_VALUE_DEPTH},
    value::Kind,
};
use std::{
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};

#[derive(Clone, Copy, Debug)]
pub(super) struct Admitted {
    pub value: Fact,
    pub incomplete: bool,
}

impl Admitted {
    fn complete(value: Fact) -> Self {
        Self {
            value,
            incomplete: false,
        }
    }
}

enum Task<'a> {
    Visit(&'a Value),
    Collection(&'a Value, usize),
    Save(&'a Value, Key),
}

/// Describes a host value without executing its methods or declaration bodies.
/// The resolver admits source identities and callable metadata only.
pub(super) fn value<'a>(
    ctx: &mut CallContext,
    facts: &mut Facts,
    value: &'a Value,
    mut resolve: impl FnMut(&mut CallContext, &mut Facts, &'a Value) -> Result<Option<Fact>>,
) -> Result<Admitted> {
    ctx.checkpoint()?;
    let mut tasks = Buffer::empty();
    let mut values: Buffer<Admitted> = Buffer::empty();
    let mut memo = Memo::new();
    tasks.push(ctx, Task::Visit(value))?;
    while let Some(task) = tasks.data.pop() {
        ctx.charge(1)?;
        match task {
            Task::Save(source, key) => {
                memo.insert(ctx, source, key, *values.data.last().unwrap())?;
            }
            Task::Collection(source, base) => {
                let children = &values.data[base..];
                let mut incomplete = false;
                for child in children {
                    ctx.charge(1)?;
                    incomplete |= child.incomplete;
                }
                let value = match &source.0 {
                    Kind::Array(_) => {
                        let mut elements = Buffer::with_capacity(ctx, children.len())?;
                        for child in children {
                            elements.push(ctx, child.value)?;
                        }
                        facts.tuple(ctx, &elements.data)?
                    }
                    Kind::Hash(hash) => {
                        let mut fields = Buffer::with_capacity(ctx, children.len())?;
                        let mut keys = Buffer::with_capacity(ctx, children.len())?;
                        for ((key, _), child) in hash.buffer.data.iter().zip(children) {
                            ctx.charge(1)?;
                            let (name, key) = match &key.0 {
                                Kind::Bytes(bytes) => {
                                    (&bytes.data, facts.string(ctx, &bytes.data)?)
                                }
                                Kind::Symbol(bytes) => {
                                    (&bytes.data, facts.symbol(ctx, &bytes.data)?)
                                }
                                _ => {
                                    return ctx.guard(ErrorKind::Type, "invalid host hash key");
                                }
                            };
                            keys.push(ctx, key)?;
                            let name = ctx.bytes(name)?;
                            fields.push(
                                ctx,
                                Field {
                                    name,
                                    value: child.value,
                                    optional: false,
                                },
                            )?;
                        }
                        let keys = facts.union(ctx, &keys.data)?;
                        let value = facts.shape_fields(
                            ctx,
                            fields,
                            false,
                            keys,
                            if hash.object {
                                HashKind::OBJECT
                            } else {
                                HashKind::PLAIN
                            },
                        )?;
                        if hash.tag.protected() {
                            facts.protected(ctx, value, hash.tag)?
                        } else {
                            value
                        }
                    }
                    _ => unreachable!(),
                };
                values.data.truncate(base);
                values.push(ctx, Admitted { value, incomplete })?;
            }
            Task::Visit(source) => {
                if source.depth() > MAX_VALUE_DEPTH {
                    return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
                }
                if let Some(key) = Key::of(source) {
                    if let Some(value) = memo.get(ctx, key)? {
                        values.push(ctx, value)?;
                        continue;
                    }
                    tasks.push(ctx, Task::Save(source, key))?;
                }
                let value = match &source.0 {
                    Kind::Nil => Atom::Nil.fact(),
                    Kind::Int(value) => facts.integer(ctx, *value)?,
                    Kind::Big(_) => Atom::Int.fact(),
                    Kind::Float(value) => facts.float(ctx, *value)?,
                    Kind::Bool(value) => facts.boolean(ctx, *value)?,
                    Kind::Bytes(value) => facts.string(ctx, &value.data)?,
                    Kind::Symbol(value) => facts.symbol(ctx, &value.data)?,
                    Kind::Builtin(value) => facts.builtin(ctx, *value)?,
                    Kind::Time(_) | Kind::Zoned(_) => Atom::Time.fact(),
                    Kind::Duration(_) => Atom::Duration.fact(),
                    Kind::Money(_) => Atom::Money.fact(),
                    Kind::Range(value) => {
                        facts.range(ctx, value.start, value.end, value.exclusive)?
                    }
                    Kind::Regex(_) => {
                        let value = ctx.import(source)?;
                        facts.regex(ctx, value)?
                    }
                    Kind::Offset(_) => {
                        let element = facts.nullable(ctx, Atom::Int.fact())?;
                        let values = facts.array(ctx, element)?;
                        facts.offset(ctx, values)?
                    }
                    Kind::Enum(_) => facts.enumeration(ctx, source)?,
                    Kind::EnumMember(member) => {
                        let enumeration = Value(Kind::Enum(member.enumeration.clone()));
                        let enumeration = facts.enumeration(ctx, &enumeration)?;
                        facts.enum_member(ctx, enumeration, member.index)?
                    }
                    Kind::Shape(shape) => {
                        let ty = facts.annotation(ctx, &shape.definition.ty, |_, _| Ok(None))?;
                        let incomplete = facts.unresolved(ty);
                        let value = facts.type_value(ctx, ty)?;
                        values.push(ctx, Admitted { value, incomplete })?;
                        continue;
                    }
                    Kind::Array(array) => {
                        tasks.push(ctx, Task::Collection(source, values.data.len()))?;
                        for item in array.buffer.data.iter().rev() {
                            ctx.charge(1)?;
                            tasks.push(ctx, Task::Visit(item))?;
                        }
                        continue;
                    }
                    Kind::Hash(hash) => {
                        tasks.push(ctx, Task::Collection(source, values.data.len()))?;
                        for (_, value) in hash.buffer.data.iter().rev() {
                            ctx.charge(1)?;
                            tasks.push(ctx, Task::Visit(value))?;
                        }
                        continue;
                    }
                    Kind::Host(_) | Kind::Function(_) | Kind::Namespace(_) | Kind::Instance(_) => {
                        let value = resolve(ctx, facts, source)?;
                        values.push(
                            ctx,
                            Admitted {
                                value: value.unwrap_or(Atom::Unknown.fact()),
                                incomplete: value.is_none(),
                            },
                        )?;
                        continue;
                    }
                };
                values.push(ctx, Admitted::complete(value))?;
            }
        }
    }
    Ok(values.data.pop().unwrap())
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Key(u8, usize);

impl Key {
    fn of(value: &Value) -> Option<Self> {
        Some(match &value.0 {
            Kind::Array(value) => Self(0, Arc::as_ptr(value) as usize),
            Kind::Hash(value) => Self(1, Arc::as_ptr(value) as usize),
            Kind::Bytes(value) => Self(2, Arc::as_ptr(value) as usize),
            Kind::Symbol(value) => Self(3, Arc::as_ptr(value) as usize),
            Kind::Host(value) => Self(4, Arc::as_ptr(value) as usize),
            Kind::Function(value) => Self(5, Arc::as_ptr(value) as usize),
            Kind::Namespace(value) => Self(6, Arc::as_ptr(value) as usize),
            Kind::Instance(value) => Self(7, Arc::as_ptr(value) as usize),
            Kind::Enum(value) => Self(8, Arc::as_ptr(value) as usize),
            Kind::EnumMember(value) => Self(9, Arc::as_ptr(value) as usize),
            Kind::Regex(value) => Self(10, Arc::as_ptr(value) as usize),
            Kind::Shape(value) => Self(11, Arc::as_ptr(value) as usize),
            Kind::Offset(value) => Self(12, Arc::as_ptr(value) as usize),
            _ => return None,
        })
    }

    fn bucket(self, capacity: usize) -> usize {
        let mut hash = DefaultHasher::new();
        self.hash(&mut hash);
        hash.finish() as usize & (capacity - 1)
    }
}

struct Entry<'a> {
    // Borrow every memoized source so its allocation cannot be reused during admission.
    _source: &'a Value,
    key: Key,
    value: Admitted,
    next: usize,
}

struct Memo<'a> {
    entries: Buffer<Entry<'a>>,
    buckets: Buffer<usize>,
}

impl<'a> Memo<'a> {
    fn new() -> Self {
        Self {
            entries: Buffer::empty(),
            buckets: Buffer::empty(),
        }
    }

    fn get(&self, ctx: &mut CallContext, key: Key) -> Result<Option<Admitted>> {
        ctx.charge(1)?;
        if self.buckets.data.is_empty() {
            return Ok(None);
        }
        let mut index = self.buckets.data[key.bucket(self.buckets.data.len())];
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

    fn insert(
        &mut self,
        ctx: &mut CallContext,
        source: &'a Value,
        key: Key,
        value: Admitted,
    ) -> Result<()> {
        if self.entries.data.len() >= self.buckets.data.len() / 2 {
            let Some(capacity) = self.buckets.data.len().max(8).checked_mul(2) else {
                return ctx.fail(ErrorKind::Memory, "checker value table size overflow");
            };
            let mut buckets = Buffer::with_capacity(ctx, capacity)?;
            ctx.charge(capacity as u64)?;
            buckets.data.resize(capacity, usize::MAX);
            ctx.charge(self.entries.data.len() as u64)?;
            for (index, entry) in self.entries.data.iter_mut().enumerate() {
                let bucket = entry.key.bucket(capacity);
                entry.next = buckets.data[bucket];
                buckets.data[bucket] = index;
            }
            self.buckets = buckets;
        }
        let bucket = key.bucket(self.buckets.data.len());
        let index = self.entries.data.len();
        self.entries.push(
            ctx,
            Entry {
                _source: source,
                key,
                value,
                next: self.buckets.data[bucket],
            },
        )?;
        self.buckets.data[bucket] = index;
        Ok(())
    }
}
