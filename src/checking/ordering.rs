use super::facts::{Atom, Fact, Facts, Node};
use crate::{CallContext, Result, Value, budget::Buffer};

pub(super) const LESS: u8 = 1;
pub(super) const EQUAL: u8 = 2;
pub(super) const GREATER: u8 = 4;
pub(super) const UNORDERED: u8 = 8;
pub(super) const ORDERED: u8 = LESS | EQUAL | GREATER;
const SAME_VALUE: u8 = 16;

#[derive(Clone, Copy, PartialEq, Eq)]
struct Pair {
    left: Fact,
    right: Fact,
}

impl Pair {
    fn bucket(self, mask: usize) -> usize {
        self.left
            .0
            .wrapping_mul(0x9e3779b1)
            .wrapping_add(self.right.0.wrapping_mul(0x85ebca77))
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
                    "checker ordering table size overflow",
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
    Alternatives(usize),
    Lexicographic {
        count: usize,
        tail: u8,
        same_length: bool,
    },
}

fn mask(value: std::cmp::Ordering) -> u8 {
    match value {
        std::cmp::Ordering::Less => LESS,
        std::cmp::Ordering::Equal => EQUAL,
        std::cmp::Ordering::Greater => GREATER,
    }
}

impl Facts {
    pub(super) fn order_result(
        &mut self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
    ) -> Result<u8> {
        ctx.checkpoint()?;
        let mut tasks = Buffer::empty();
        let mut values = Buffer::empty();
        let mut memo = Memo::new();
        tasks.push(ctx, Task::Visit(Pair { left, right }))?;
        while let Some(task) = tasks.data.pop() {
            ctx.charge(1)?;
            let value = match task {
                Task::Save(pair) => {
                    memo.insert(ctx, pair, *values.data.last().unwrap())?;
                    continue;
                }
                Task::Alternatives(count) => {
                    let start = values.data.len() - count;
                    let mut result = 0;
                    for &value in &values.data[start..] {
                        ctx.charge(1)?;
                        result |= value;
                    }
                    values.data.truncate(start);
                    result
                }
                Task::Lexicographic {
                    count,
                    tail,
                    same_length,
                } => {
                    let start = values.data.len() - count;
                    let mut result = EQUAL;
                    let mut shared = same_length;
                    for &value in values.data[start..].iter().rev() {
                        ctx.charge(1)?;
                        shared &= value & SAME_VALUE != 0;
                        if result & EQUAL != 0 {
                            result = (result & !EQUAL) | (value & !SAME_VALUE);
                        }
                    }
                    if result & EQUAL != 0 {
                        result = (result & !EQUAL) | tail;
                    }
                    // Overlapping element facts can describe aliased arrays even
                    // when their shapes differ or a shared child is not comparable.
                    if shared {
                        result |= EQUAL | SAME_VALUE;
                    }
                    values.data.truncate(start);
                    result
                }
                Task::Visit(pair) => {
                    if let Some(value) = memo.get(ctx, pair)? {
                        values.push(ctx, value)?;
                        continue;
                    }
                    tasks.push(ctx, Task::Save(pair))?;
                    let Pair { left, right } = pair;
                    match (self.node(left), self.node(right)) {
                        (Node::Union(arms), _) => {
                            tasks.push(ctx, Task::Alternatives(arms.data.len()))?;
                            for &left in &arms.data {
                                ctx.charge(1)?;
                                tasks.push(ctx, Task::Visit(Pair { left, ..pair }))?;
                            }
                            continue;
                        }
                        (_, Node::Union(arms)) => {
                            tasks.push(ctx, Task::Alternatives(arms.data.len()))?;
                            for &right in &arms.data {
                                ctx.charge(1)?;
                                tasks.push(ctx, Task::Visit(Pair { right, ..pair }))?;
                            }
                            continue;
                        }
                        (Node::Tuple(a), Node::Tuple(b)) => {
                            tasks.push(
                                ctx,
                                Task::Lexicographic {
                                    count: a.data.len().min(b.data.len()),
                                    tail: mask(a.data.len().cmp(&b.data.len())),
                                    same_length: a.data.len() == b.data.len(),
                                },
                            )?;
                            for (&left, &right) in a.data.iter().zip(&b.data) {
                                ctx.charge(1)?;
                                tasks.push(ctx, Task::Visit(Pair { left, right }))?;
                            }
                            continue;
                        }
                        (Node::Tuple(_) | Node::Array(_), Node::Tuple(_) | Node::Array(_)) => {
                            ORDERED | UNORDERED | SAME_VALUE
                        }
                        _ => {
                            let order = self.order_scalar(ctx, left, right)?;
                            if self.overlaps(ctx, left, right)? {
                                order | SAME_VALUE
                            } else {
                                order
                            }
                        }
                    }
                }
            };
            values.push(ctx, value)?;
        }
        assert_eq!(values.data.len(), 1);
        Ok(values.data[0] & !SAME_VALUE)
    }

    fn order_scalar(&self, ctx: &mut CallContext, left: Fact, right: Fact) -> Result<u8> {
        if left == Atom::Never.fact() || right == Atom::Never.fact() {
            return Ok(0);
        }
        let number = |value| match self.node(value) {
            Node::Integer(n) => Some(Value::int(*n)),
            Node::Float(n) => Some(Value::float(f64::from_bits(*n))),
            _ => None,
        };
        if let (Some(a), Some(b)) = (number(left), number(right)) {
            return Ok(crate::ops::compare(ctx, &a, &b)?.map_or(UNORDERED, mask));
        }
        match (self.node(left), self.node(right)) {
            (Node::Boolean(a), Node::Boolean(b)) => return Ok(mask(a.cmp(b))),
            (Node::String(a), Node::String(b)) | (Node::Symbol(a), Node::Symbol(b)) => {
                let a = a.as_bytes().unwrap();
                let b = b.as_bytes().unwrap();
                ctx.work_bytes(a.len().min(b.len()).saturating_add(1))?;
                return Ok(mask(a.cmp(b)));
            }
            (Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_) | Node::Nominal { .. }, _)
            | (_, Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_) | Node::Nominal { .. }) => {
                return Ok(ORDERED | UNORDERED);
            }
            _ => (),
        }
        if matches!(self.node(left),Node::Float(bits) if f64::from_bits(*bits).is_nan())
            || matches!(self.node(right),Node::Float(bits) if f64::from_bits(*bits).is_nan())
        {
            return Ok(UNORDERED);
        }
        Ok(match (self.atom(left), self.atom(right)) {
            (Some(Atom::Nil), Some(Atom::Nil)) => EQUAL,
            (Some(Atom::Int), Some(Atom::Int)) => ORDERED,
            (Some(Atom::Int | Atom::Float), Some(Atom::Int | Atom::Float)) => {
                if left == Atom::Float.fact() || right == Atom::Float.fact() {
                    ORDERED | UNORDERED
                } else {
                    ORDERED
                }
            }
            (Some(a), Some(b))
                if a == b
                    && matches!(
                        a,
                        Atom::Bool | Atom::String | Atom::Symbol | Atom::Time | Atom::Duration
                    ) =>
            {
                ORDERED
            }
            (Some(Atom::Money), Some(Atom::Money)) => ORDERED | UNORDERED,
            _ => UNORDERED,
        })
    }
}
