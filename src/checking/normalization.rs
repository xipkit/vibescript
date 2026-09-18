use super::{
    facts::{Atom, Fact, Facts, Field, HashKind, Node},
    relation::Relation,
};
use crate::{CallContext, ErrorKind, Result, budget::Buffer, value::Kind};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    actual: Option<Fact>,
    expected: Fact,
}

impl Key {
    fn bucket(self, mask: usize) -> usize {
        self.actual
            .map_or(usize::MAX, |value| value.0)
            .wrapping_mul(0x9e3779b1)
            .wrapping_add(self.expected.0.wrapping_mul(0x85ebca77))
            & mask
    }
}

#[derive(Clone, Copy)]
struct Normalized {
    value: Fact,
    unchanged: bool,
    changed: bool,
}

impl Normalized {
    fn value(value: Fact) -> Self {
        Self {
            value,
            unchanged: value != Atom::Never.fact(),
            changed: false,
        }
    }
}

struct Entry {
    key: Key,
    value: Normalized,
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

    fn get(&self, ctx: &mut CallContext, key: Key) -> Result<Option<Normalized>> {
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

    fn insert(&mut self, ctx: &mut CallContext, key: Key, value: Normalized) -> Result<()> {
        if self.entries.data.len() >= self.buckets.data.len() / 2 {
            let Some(capacity) = self.buckets.data.len().max(8).checked_mul(2) else {
                return ctx.fail(
                    ErrorKind::Memory,
                    "checker normalization table size overflow",
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
    Union(usize),
    Array,
    Tuple(usize),
    Hash(Fact, HashKind),
    Shape(Buffer<Field>, bool, Fact, HashKind),
    Protected(crate::hash::Tag),
    Options(Fact, Buffer<Fact>, usize, Normalized),
    Candidate(Fact, Buffer<Fact>, usize, Normalized, bool),
}

impl Facts {
    /// Infers values after a successful runtime boundary normalization.
    pub fn normalized(
        &mut self,
        ctx: &mut CallContext,
        actual: Fact,
        expected: Fact,
    ) -> Result<Fact> {
        self.normalize_facts(ctx, Some(actual), expected)
    }

    /// Converts annotation contracts to their successful runtime value domains.
    pub fn value_domain(&mut self, ctx: &mut CallContext, expected: Fact) -> Result<Fact> {
        ctx.checkpoint()?;
        if !self.normalizes(expected) && self.depth(expected) == 0 {
            return Ok(expected);
        }
        self.normalize_facts(ctx, None, expected)
    }

    fn normalize_facts(
        &mut self,
        ctx: &mut CallContext,
        actual: Option<Fact>,
        expected: Fact,
    ) -> Result<Fact> {
        ctx.checkpoint()?;
        let mut tasks = Buffer::empty();
        let mut values: Buffer<Normalized> = Buffer::empty();
        let mut memo = Memo::new();
        tasks.push(ctx, Task::Visit(Key { actual, expected }))?;
        while let Some(task) = tasks.data.pop() {
            ctx.charge(1)?;
            let result = match task {
                Task::Save(key) => {
                    memo.insert(ctx, key, *values.data.last().unwrap())?;
                    continue;
                }
                Task::Union(count) => {
                    let start = values.data.len() - count;
                    let mut result = Normalized::value(Atom::Never.fact());
                    for &value in &values.data[start..] {
                        self.normalization_join(ctx, &mut result, value)?;
                    }
                    values.data.truncate(start);
                    result
                }
                Task::Array => {
                    let item = values.data.pop().unwrap();
                    let value = if item.value == Atom::Never.fact() {
                        self.tuple(ctx, &[])?
                    } else {
                        self.array(ctx, item.value)?
                    };
                    Normalized {
                        value,
                        unchanged: true,
                        changed: item.changed,
                    }
                }
                Task::Tuple(count) => {
                    let start = values.data.len() - count;
                    let mut items = Buffer::with_capacity(ctx, count)?;
                    let mut unchanged = true;
                    let mut changed = false;
                    let mut possible = true;
                    for item in &values.data[start..] {
                        items.push(ctx, item.value)?;
                        possible &= item.value != Atom::Never.fact();
                        unchanged &= item.unchanged;
                        changed |= item.changed;
                    }
                    values.data.truncate(start);
                    if possible {
                        Normalized {
                            value: self.tuple(ctx, &items.data)?,
                            unchanged,
                            changed,
                        }
                    } else {
                        Normalized::value(Atom::Never.fact())
                    }
                }
                Task::Hash(keys, plain) => {
                    let item = values.data.pop().unwrap();
                    let value = self.hash_kind(ctx, keys, item.value, plain)?;
                    Normalized {
                        value,
                        unchanged: true,
                        changed: item.changed,
                    }
                }
                Task::Shape(mut fields, open, keys, plain) => {
                    let start = values.data.len() - fields.data.len();
                    let mut output = Buffer::with_capacity(ctx, fields.data.len())?;
                    let mut possible = true;
                    let mut unchanged = true;
                    let mut changed = false;
                    for (mut field, item) in fields.data.drain(..).zip(&values.data[start..]) {
                        ctx.charge(1)?;
                        if item.value == Atom::Never.fact() {
                            possible &= field.optional;
                            continue;
                        }
                        unchanged &= field.optional || item.unchanged;
                        changed |= item.changed;
                        field.value = item.value;
                        output.push(ctx, field)?;
                    }
                    values.data.truncate(start);
                    if possible {
                        Normalized {
                            value: self.shape_fields(ctx, output, open, keys, plain)?,
                            unchanged,
                            changed,
                        }
                    } else {
                        Normalized::value(Atom::Never.fact())
                    }
                }
                Task::Protected(tag) => {
                    let item = values.data.pop().unwrap();
                    let mut variants = Buffer::empty();
                    if item.unchanged {
                        for i in 0..self.arm_count(item.value) {
                            let arm = self.arm(item.value, i);
                            if arm != Atom::Never.fact() {
                                let protected = self.protected(ctx, arm, tag)?;
                                variants.push(ctx, protected)?;
                            }
                        }
                    }
                    if item.changed {
                        variants.push(ctx, item.value)?;
                    }
                    Normalized {
                        value: self.union(ctx, &variants.data)?,
                        ..item
                    }
                }
                Task::Options(actual, options, mut index, accumulated) => {
                    let mut next = None;
                    while let Some(&option) = options.data.get(index) {
                        index += 1;
                        let relation = self.relation(ctx, actual, option)?;
                        if relation != Relation::Rejected || self.overlaps(ctx, actual, option)? {
                            next = Some((option, relation == Relation::Accepted));
                            break;
                        }
                    }
                    if let Some((option, stops)) = next {
                        tasks.push(
                            ctx,
                            Task::Candidate(actual, options, index, accumulated, stops),
                        )?;
                        tasks.push(
                            ctx,
                            Task::Visit(Key {
                                actual: Some(actual),
                                expected: option,
                            }),
                        )?;
                        continue;
                    }
                    accumulated
                }
                Task::Candidate(actual, options, index, mut accumulated, stops) => {
                    let candidate = values.data.pop().unwrap();
                    self.normalization_join(ctx, &mut accumulated, candidate)?;
                    if stops {
                        accumulated
                    } else {
                        tasks.push(ctx, Task::Options(actual, options, index, accumulated))?;
                        continue;
                    }
                }
                Task::Visit(key) => {
                    if let Some(value) = memo.get(ctx, key)? {
                        values.push(ctx, value)?;
                        continue;
                    }
                    tasks.push(ctx, Task::Save(key))?;
                    let expected = key.expected;
                    if let Some(actual) = key.actual {
                        if actual == Atom::Never.fact() {
                            values.push(ctx, Normalized::value(actual))?;
                            continue;
                        }
                        let relation = self.relation(ctx, actual, expected)?;
                        if relation == Relation::Rejected
                            && !self.overlaps(ctx, actual, expected)?
                        {
                            values.push(ctx, Normalized::value(Atom::Never.fact()))?;
                            continue;
                        }
                        if relation == Relation::Accepted
                            && (!self.normalizes(expected) || self.enum_nominal(actual).is_some())
                        {
                            let value = if self.normalizes(actual) {
                                self.value_domain(ctx, actual)?
                            } else {
                                actual
                            };
                            values.push(ctx, Normalized::value(value))?;
                            continue;
                        }
                        if let Node::Union(arms) = self.node(actual) {
                            tasks.push(ctx, Task::Union(arms.data.len()))?;
                            for &arm in arms.data.iter().rev() {
                                tasks.push(
                                    ctx,
                                    Task::Visit(Key {
                                        actual: Some(arm),
                                        expected,
                                    }),
                                )?;
                            }
                            continue;
                        }
                        if let Node::Protected(shape, tag) = self.node(actual) {
                            tasks.push(ctx, Task::Protected(*tag))?;
                            tasks.push(
                                ctx,
                                Task::Visit(Key {
                                    actual: Some(*shape),
                                    expected,
                                }),
                            )?;
                            continue;
                        }
                    }
                    if let Some(enumeration) = self.enum_contract(expected) {
                        let mut value = None;
                        if let Some(actual) = key.actual {
                            if let Node::Symbol(symbol) = self.node(actual) {
                                let Node::Enumeration { value: parent, .. } =
                                    self.node(enumeration)
                                else {
                                    unreachable!()
                                };
                                let Kind::Enum(parent) = &parent.0 else {
                                    unreachable!()
                                };
                                if let Some(index) =
                                    parent.lookup_symbol(ctx, symbol.as_bytes().unwrap())?
                                {
                                    value = Some(self.enum_member(ctx, enumeration, index)?);
                                }
                            }
                        }
                        let value = if let Some(value) = value {
                            value
                        } else {
                            self.enum_members(ctx, enumeration)?
                        };
                        let symbol = key
                            .actual
                            .is_some_and(|actual| self.atom(actual) == Some(Atom::Symbol));
                        let same = key.actual == Some(expected);
                        Normalized {
                            value,
                            unchanged: !symbol,
                            changed: !same,
                        }
                    } else {
                        match self.node(expected) {
                            Node::Choice(arms) if key.actual.is_some() => {
                                let mut options = Buffer::with_capacity(ctx, arms.data.len())?;
                                for any in [false, true] {
                                    for &arm in &arms.data {
                                        ctx.charge(1)?;
                                        if (arm == Atom::Any.fact()) == any {
                                            options.push(ctx, arm)?;
                                        }
                                    }
                                }
                                tasks.push(
                                    ctx,
                                    Task::Options(
                                        key.actual.unwrap(),
                                        options,
                                        0,
                                        Normalized::value(Atom::Never.fact()),
                                    ),
                                )?;
                                continue;
                            }
                            Node::Choice(arms) | Node::Union(arms) => {
                                tasks.push(ctx, Task::Union(arms.data.len()))?;
                                for &expected in arms.data.iter().rev() {
                                    tasks.push(ctx, Task::Visit(Key { expected, ..key }))?;
                                }
                                continue;
                            }
                            Node::Array(element) => {
                                let element = *element;
                                match key.actual.map(|actual| self.node(actual)) {
                                    Some(Node::Tuple(items)) => {
                                        tasks.push(ctx, Task::Tuple(items.data.len()))?;
                                        for &actual in items.data.iter().rev() {
                                            tasks.push(
                                                ctx,
                                                Task::Visit(Key {
                                                    actual: Some(actual),
                                                    expected: element,
                                                }),
                                            )?;
                                        }
                                    }
                                    actual => {
                                        let actual = if let Some(Node::Array(item)) = actual {
                                            Some(*item)
                                        } else {
                                            None
                                        };
                                        tasks.push(ctx, Task::Array)?;
                                        tasks.push(
                                            ctx,
                                            Task::Visit(Key {
                                                actual,
                                                expected: element,
                                            }),
                                        )?;
                                    }
                                }
                                continue;
                            }
                            Node::Hash(keys, element, plain) => {
                                let (keys, element, plain) = (*keys, *element, *plain);
                                match key.actual.map(|actual| self.node(actual)) {
                                    Some(Node::Shape(fields, false, keys, plain)) => {
                                        let mut output =
                                            Buffer::with_capacity(ctx, fields.data.len())?;
                                        for field in &fields.data {
                                            output.push(
                                                ctx,
                                                Field {
                                                    name: field.name.clone(),
                                                    value: field.value,
                                                    optional: field.optional,
                                                },
                                            )?;
                                        }
                                        let mut children = Buffer::empty();
                                        for field in fields.data.iter().rev() {
                                            children.push(
                                                ctx,
                                                Key {
                                                    actual: Some(field.value),
                                                    expected: element,
                                                },
                                            )?;
                                        }
                                        tasks
                                            .push(ctx, Task::Shape(output, false, *keys, *plain))?;
                                        for child in children.data {
                                            tasks.push(ctx, Task::Visit(child))?;
                                        }
                                    }
                                    Some(Node::Hash(actual_keys, actual_value, actual_plain)) => {
                                        tasks.push(ctx, Task::Hash(*actual_keys, *actual_plain))?;
                                        tasks.push(
                                            ctx,
                                            Task::Visit(Key {
                                                actual: Some(*actual_value),
                                                expected: element,
                                            }),
                                        )?;
                                    }
                                    _ => {
                                        let keys = self.key_domain(ctx, keys)?;
                                        tasks.push(ctx, Task::Hash(keys, plain))?;
                                        tasks.push(
                                            ctx,
                                            Task::Visit(Key {
                                                actual: None,
                                                expected: element,
                                            }),
                                        )?;
                                    }
                                }
                                continue;
                            }
                            Node::Shape(_, open, keys, plain) => {
                                let (open, mut keys, plain) = (*open, *keys, *plain);
                                if key.actual.is_none() {
                                    keys = self.key_domain(ctx, keys)?;
                                }
                                let concrete = key.actual.filter(|&actual| {
                                    matches!(self.node(actual), Node::Shape(_, false, _, _))
                                });
                                let template = concrete.unwrap_or(expected);
                                let Node::Shape(fields, source_open, source_keys, source_plain) =
                                    self.node(template)
                                else {
                                    unreachable!()
                                };
                                let mut output = Buffer::with_capacity(ctx, fields.data.len())?;
                                let mut children = Buffer::with_capacity(ctx, fields.data.len())?;
                                for field in &fields.data {
                                    let actual = concrete.map(|_| field.value);
                                    let target = if concrete.is_some() {
                                        self.selected_field(
                                            ctx,
                                            expected,
                                            field.name.as_bytes().unwrap(),
                                        )?
                                        .map_or(Atom::Any.fact(), |(value, _)| value)
                                    } else {
                                        field.value
                                    };
                                    output.push(
                                        ctx,
                                        Field {
                                            name: field.name.clone(),
                                            value: field.value,
                                            optional: field.optional,
                                        },
                                    )?;
                                    children.push(
                                        ctx,
                                        Key {
                                            actual,
                                            expected: target,
                                        },
                                    )?;
                                }
                                let (open, keys, plain) = if concrete.is_some() {
                                    (*source_open, *source_keys, *source_plain)
                                } else {
                                    (open, keys, plain)
                                };
                                tasks.push(ctx, Task::Shape(output, open, keys, plain))?;
                                for &child in children.data.iter().rev() {
                                    tasks.push(ctx, Task::Visit(child))?;
                                }
                                continue;
                            }
                            Node::Tuple(items) => {
                                tasks.push(ctx, Task::Tuple(items.data.len()))?;
                                for &expected in items.data.iter().rev() {
                                    tasks.push(
                                        ctx,
                                        Task::Visit(Key {
                                            actual: None,
                                            expected,
                                        }),
                                    )?;
                                }
                                continue;
                            }
                            _ => Normalized::value(expected),
                        }
                    }
                }
            };
            values.push(ctx, result)?;
        }
        assert_eq!(values.data.len(), 1);
        Ok(values.data[0].value)
    }

    fn normalization_join(
        &mut self,
        ctx: &mut CallContext,
        result: &mut Normalized,
        other: Normalized,
    ) -> Result<()> {
        result.value = self.union(ctx, &[result.value, other.value])?;
        result.unchanged |= other.unchanged;
        result.changed |= other.changed;
        Ok(())
    }

    fn key_domain(&mut self, ctx: &mut CallContext, expected: Fact) -> Result<Fact> {
        if self.string_key(expected) == Some(true) {
            return Ok(Atom::String.fact());
        }
        if self.string_key(expected) == Some(false) {
            return Ok(Atom::Never.fact());
        }
        Ok(
            if self.relation(ctx, Atom::String.fact(), expected)? == Relation::Rejected {
                Atom::Never.fact()
            } else {
                Atom::String.fact()
            },
        )
    }
}
