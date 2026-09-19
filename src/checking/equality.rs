use super::facts::{Atom, Fact, Facts, HashKind, Node};
use super::slots::Slots;
use crate::{CallContext, Result, Value, budget::Buffer};

const NO: u8 = 1;
const YES: u8 = 2;
const MAYBE: u8 = NO | YES;
const LIMIT: u8 = 4;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Pair {
    left: Fact,
    right: Fact,
    root: bool,
    depth: usize,
}

impl Pair {
    fn bucket(self, mask: usize) -> usize {
        self.left
            .0
            .wrapping_mul(0x9e3779b1)
            .wrapping_add(self.right.0.wrapping_mul(0x85ebca77))
            .wrapping_add(usize::from(self.root))
            .wrapping_add(self.depth.wrapping_mul(0xc2b2ae35))
            & mask
    }
}

struct Entry {
    pair: Pair,
    value: u8,
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

    fn get(&self, ctx: &mut CallContext, pair: Pair) -> Result<Option<u8>> {
        ctx.charge(1)?;
        if self.buckets.data.is_empty() {
            return Ok(None);
        }
        let mut index = self.buckets.data[pair.bucket(self.buckets.data.len() - 1)];
        while index != usize::MAX {
            ctx.charge(1)?;
            let entry = &self.entries.data[index];
            if entry.pair == pair {
                return Ok(Some(entry.value));
            }
            index = entry.next;
        }
        Ok(None)
    }

    fn insert(&mut self, ctx: &mut CallContext, pair: Pair, value: u8) -> Result<()> {
        if self.entries.data.len() >= self.buckets.data.len() / 2 {
            let Some(capacity) = self.buckets.data.len().max(8).checked_mul(2) else {
                return ctx.fail(
                    crate::ErrorKind::Memory,
                    "checker equality table size overflow",
                );
            };
            let mut buckets = Buffer::with_capacity(ctx, capacity)?;
            ctx.charge(capacity as u64)?;
            buckets.data.resize(capacity, usize::MAX);
            ctx.charge(self.entries.data.len() as u64)?;
            for (index, entry) in self.entries.data.iter_mut().enumerate() {
                let bucket = entry.pair.bucket(capacity - 1);
                entry.next = buckets.data[bucket];
                buckets.data[bucket] = index;
            }
            self.buckets = buckets;
        }
        let bucket = pair.bucket(self.buckets.data.len() - 1);
        let index = self.entries.data.len();
        self.entries.push(
            ctx,
            Entry {
                pair,
                value,
                next: self.buckets.data[bucket],
            },
        )?;
        self.buckets.data[bucket] = index;
        Ok(())
    }
}

enum Task {
    Visit(Pair),
    Save(Pair),
    All(usize, bool),
    Hash(usize, bool, bool),
    Alternatives(usize),
}

impl Facts {
    pub(super) fn set_equal(
        &mut self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
    ) -> Result<Fact> {
        let value = self.compare_values(ctx, left, right, true)?;
        self.equality_fact(ctx, value)
    }

    pub(super) fn value_equal(
        &mut self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
    ) -> Result<(Fact, bool)> {
        let value = self.compare_values(ctx, left, right, false)?;
        Ok((self.equality_fact(ctx, value)?, value & LIMIT != 0))
    }

    fn equality_fact(&mut self, ctx: &mut CallContext, value: u8) -> Result<Fact> {
        match value & MAYBE {
            0 => Ok(Atom::Never.fact()),
            NO => self.boolean(ctx, false),
            YES => self.boolean(ctx, true),
            _ => Ok(Atom::Bool.fact()),
        }
    }

    fn compare_values(
        &mut self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
        set: bool,
    ) -> Result<u8> {
        ctx.checkpoint()?;
        let mut tasks = Buffer::empty();
        let mut values = Buffer::empty();
        let mut memo = Memo::new();
        tasks.push(
            ctx,
            Task::Visit(Pair {
                left,
                right,
                root: set,
                depth: 0,
            }),
        )?;
        while let Some(task) = tasks.data.pop() {
            ctx.charge(1)?;
            let value = match task {
                Task::Save(pair) => {
                    memo.insert(ctx, pair, *values.data.last().unwrap())?;
                    continue;
                }
                Task::All(count, _) | Task::Hash(count, ..) | Task::Alternatives(count) => {
                    let start = values.data.len() - count;
                    let all = !matches!(task, Task::Alternatives(_));
                    let mut result = if all { YES } else { 0 };
                    for &value in &values.data[start..] {
                        ctx.charge(1)?;
                        result = if !all {
                            result | value
                        } else if matches!(task, Task::All(_, true)) {
                            // Array comparisons stop before later elements after a mismatch or guard.
                            (result & (NO | LIMIT)) | if result & YES != 0 { value } else { 0 }
                        } else if result == 0 || value == 0 {
                            0
                        } else {
                            ((result | value) & (NO | LIMIT)) | ((result & value) & YES)
                        };
                    }
                    values.data.truncate(start);
                    if let Task::Hash(_, missing, uncertain_kind) = task {
                        // Shapes sort keys, so retain both possible outcomes when insertion order
                        // determines whether a missing/different value or a nesting guard is first.
                        if missing {
                            result = (result & LIMIT) | NO;
                        }
                        if uncertain_kind {
                            result |= NO;
                        }
                    }
                    result
                }
                Task::Visit(pair) => {
                    if !set && pair.depth > crate::budget::MAX_VALUE_DEPTH {
                        values.push(ctx, LIMIT)?;
                        continue;
                    }
                    if let Some(value) = memo.get(ctx, pair)? {
                        values.push(ctx, value)?;
                        continue;
                    }
                    tasks.push(ctx, Task::Save(pair))?;
                    let Pair {
                        left,
                        right,
                        root,
                        depth,
                    } = pair;
                    let child_depth = if set { 0 } else { depth + 1 };
                    let hash_kind = |value| match self.node(value) {
                        Node::Hash(_, _, kind) | Node::Shape(_, _, _, kind) => Some(*kind),
                        _ => None,
                    };
                    if !set {
                        if let (Some(a), Some(b)) = (hash_kind(left), hash_kind(right)) {
                            if a != b && a != HashKind::Any && b != HashKind::Any {
                                values.push(ctx, NO)?;
                                continue;
                            }
                        }
                    }
                    match (self.node(left), self.node(right)) {
                        (Node::Protected(value, _), _) => {
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    left: *value,
                                    ..pair
                                }),
                            )?;
                            continue;
                        }
                        (_, Node::Protected(value, _)) => {
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    right: *value,
                                    ..pair
                                }),
                            )?;
                            continue;
                        }
                        (Node::Union(arms), _) => {
                            tasks.push(ctx, Task::Alternatives(arms.data.len()))?;
                            for &arm in &arms.data {
                                ctx.charge(1)?;
                                tasks.push(ctx, Task::Visit(Pair { left: arm, ..pair }))?;
                            }
                            continue;
                        }
                        (_, Node::Union(arms)) => {
                            tasks.push(ctx, Task::Alternatives(arms.data.len()))?;
                            for &arm in &arms.data {
                                ctx.charge(1)?;
                                tasks.push(ctx, Task::Visit(Pair { right: arm, ..pair }))?;
                            }
                            continue;
                        }
                        (Node::Tuple(a), Node::Tuple(b)) => {
                            if a.data.len() != b.data.len() {
                                NO
                            } else {
                                tasks.push(ctx, Task::All(a.data.len(), !set))?;
                                for (&left, &right) in a.data.iter().zip(&b.data).rev() {
                                    ctx.charge(1)?;
                                    tasks.push(
                                        ctx,
                                        Task::Visit(Pair {
                                            left,
                                            right,
                                            root: false,
                                            depth: child_depth,
                                        }),
                                    )?;
                                }
                                continue;
                            }
                        }
                        (Node::Shape(a, false, _, ak), Node::Shape(b, false, _, bk))
                            if !set || (*ak == HashKind::Plain && *bk == HashKind::Plain) =>
                        {
                            let mut required = true;
                            for field in a.data.iter().chain(&b.data) {
                                ctx.charge(1)?;
                                required &= !field.optional;
                            }
                            if !required {
                                self.uncertain_equality(ctx, left, right, depth, set)?
                            } else if a.data.len() != b.data.len() {
                                NO
                            } else {
                                let mut pairs = Buffer::empty();
                                let mut missing = false;
                                for field in &a.data {
                                    ctx.charge(1)?;
                                    if let Some((value, _)) = self.selected_field(
                                        ctx,
                                        right,
                                        field.name.as_bytes().unwrap(),
                                    )? {
                                        pairs.push(
                                            ctx,
                                            Pair {
                                                left: field.value,
                                                right: value,
                                                root: false,
                                                depth: child_depth,
                                            },
                                        )?;
                                    } else {
                                        missing = true;
                                    }
                                }
                                if missing && set {
                                    NO
                                } else {
                                    tasks.push(
                                        ctx,
                                        Task::Hash(
                                            pairs.data.len(),
                                            missing,
                                            !set && (*ak == HashKind::Any || *bk == HashKind::Any),
                                        ),
                                    )?;
                                    for pair in pairs.data {
                                        tasks.push(ctx, Task::Visit(pair))?;
                                    }
                                    continue;
                                }
                            }
                        }
                        _ => {
                            let value = self.set_scalar_equal(ctx, left, right, root)?;
                            if value == MAYBE {
                                self.uncertain_equality(ctx, left, right, depth, set)?
                            } else {
                                value
                            }
                        }
                    }
                }
            };
            values.push(ctx, value)?;
        }
        assert_eq!(values.data.len(), 1);
        Ok(values.data[0])
    }

    fn uncertain_equality(
        &self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
        depth: usize,
        set: bool,
    ) -> Result<u8> {
        Ok(
            if !set
                && self.equality_may_exceed_depth(ctx, left, depth)?
                && self.equality_may_exceed_depth(ctx, right, depth)?
            {
                MAYBE | LIMIT
            } else {
                MAYBE
            },
        )
    }

    fn equality_may_exceed_depth(
        &self,
        ctx: &mut CallContext,
        value: Fact,
        depth: usize,
    ) -> Result<bool> {
        if self.depth(value).saturating_add(depth) > crate::budget::MAX_VALUE_DEPTH {
            return Ok(true);
        }
        let mut pending = Buffer::empty();
        let mut visited = Slots::new(self.len(), false);
        pending.push(ctx, value)?;
        while let Some(value) = pending.data.pop() {
            ctx.charge(1)?;
            if visited.get(ctx, value.0)? {
                continue;
            }
            visited.set(ctx, value.0, true)?;
            match self.node(value) {
                Node::Atom(Atom::Unknown | Atom::Any)
                | Node::Named(_)
                | Node::Nominal { .. }
                | Node::Choice(_)
                | Node::Shape(_, true, _, _) => return Ok(true),
                Node::Array(value) | Node::Hash(_, value, _) | Node::Protected(value, _) => {
                    pending.push(ctx, *value)?;
                }
                Node::Tuple(values) | Node::Union(values) => pending.extend(ctx, &values.data)?,
                Node::Shape(fields, ..) => {
                    for field in &fields.data {
                        ctx.charge(1)?;
                        pending.push(ctx, field.value)?;
                    }
                }
                _ => (),
            }
        }
        Ok(false)
    }

    fn set_scalar_equal(
        &self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
        root: bool,
    ) -> Result<u8> {
        if left == Atom::Never.fact() || right == Atom::Never.fact() {
            return Ok(0);
        }
        let truth = |value| if value { YES } else { NO };
        if root
            && matches!(
                (self.atom(left), self.atom(right)),
                (Some(Atom::Int), Some(Atom::Float)) | (Some(Atom::Float), Some(Atom::Int))
            )
        {
            return Ok(NO);
        }
        let number = |value| match self.node(value) {
            Node::Integer(value) => Some(Value::int(*value)),
            Node::Float(value) => Some(Value::float(f64::from_bits(*value))),
            _ => None,
        };
        let nan =
            |value| matches!(self.node(value), Node::Float(bits) if f64::from_bits(*bits).is_nan());
        if root && nan(left) && nan(right) {
            return Ok(YES);
        }
        if !root && (nan(left) || nan(right)) {
            return Ok(NO);
        }
        if let (Some(a), Some(b)) = (number(left), number(right)) {
            return Ok(truth(crate::ops::equal(ctx, &a, &b, 0)?));
        }
        Ok(self.definitely_equal(left, right).map_or(MAYBE, truth))
    }
}
