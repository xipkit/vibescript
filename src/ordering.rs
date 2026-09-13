use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, MAX_VALUE_DEPTH},
    iteration::Progress,
    ops,
    sort::{Action, Sort},
    value::Kind,
};
use std::{cmp::Ordering, sync::Arc};

struct Memo {
    left: usize,
    right: usize,
    order: Option<Ordering>,
}

struct Compare {
    memo: Buffer<Memo>,
    buckets: Buffer<usize>,
    next: usize,
}

impl Compare {
    fn new() -> Self {
        Self {
            memo: Buffer::empty(),
            buckets: Buffer::empty(),
            next: 0,
        }
    }

    fn clear(&mut self) {
        self.memo.data.clear();
        self.buckets.data.fill(0);
        self.next = 0;
    }

    fn hash(left: usize, right: usize) -> usize {
        let mut hash =
            (left as u64).wrapping_mul(0x9e3779b97f4a7c15) ^ (right as u64).rotate_left(27);
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51afd7ed558ccd);
        (hash ^ (hash >> 33)) as usize
    }

    fn bucket(&self, left: usize, right: usize) -> usize {
        let mask = self.buckets.data.len() - 1;
        let mut slot = Self::hash(left, right) & mask;
        // At most 256 entries occupy at most 512 buckets. The enclosing logical
        // comparison is charged once, independently of heap-address collisions.
        loop {
            let entry = self.buckets.data[slot];
            if entry == 0 {
                return slot;
            }
            let memo = &self.memo.data[entry - 1];
            if memo.left == left && memo.right == right {
                return slot;
            }
            slot = (slot + 1) & mask;
        }
    }

    fn remember(&mut self, ctx: &mut CallContext, memo: Memo) -> Result<()> {
        ctx.charge(1)?;
        let index = if self.memo.data.len() < 256 {
            let len = self.memo.data.len();
            if 2 * (len + 1) > self.buckets.data.len() {
                let capacity = self.buckets.data.len().max(8) * 2;
                let mut buckets = Buffer::with_capacity(ctx, capacity)?;
                ctx.work_bytes(capacity * size_of::<usize>())?;
                buckets.data.resize(capacity, 0);
                self.buckets = buckets;
                for (index, memo) in self.memo.data.iter().enumerate() {
                    ctx.charge(1)?;
                    let slot = self.bucket(memo.left, memo.right);
                    self.buckets.data[slot] = index + 1;
                }
            }
            self.memo.push(ctx, memo)?;
            len
        } else {
            let index = self.next;
            let old = &self.memo.data[index];
            let mut hole = self.bucket(old.left, old.right);
            let mask = self.buckets.data.len() - 1;
            self.buckets.data[hole] = 0;
            let mut slot = (hole + 1) & mask;
            while self.buckets.data[slot] != 0 {
                let entry = self.buckets.data[slot];
                let memo = &self.memo.data[entry - 1];
                let home = Self::hash(memo.left, memo.right) & mask;
                if (slot.wrapping_sub(home) & mask) >= (slot.wrapping_sub(hole) & mask) {
                    self.buckets.data[hole] = entry;
                    self.buckets.data[slot] = 0;
                    hole = slot;
                }
                slot = (slot + 1) & mask;
            }
            self.memo.data[index] = memo;
            self.next = (index + 1) % 256;
            index
        };
        let memo = &self.memo.data[index];
        let slot = self.bucket(memo.left, memo.right);
        self.buckets.data[slot] = index + 1;
        Ok(())
    }

    fn order(
        &mut self,
        ctx: &mut CallContext,
        a: &Value,
        b: &Value,
        depth: usize,
    ) -> Result<Option<Ordering>> {
        ctx.charge(1)?;
        if depth > MAX_VALUE_DEPTH {
            return ctx.fail(ErrorKind::Recursion, "value nesting too deep");
        }
        match (&a.0, &b.0) {
            (Kind::Array(a), Kind::Array(b)) => {
                if Arc::ptr_eq(a, b) {
                    return Ok(Some(Ordering::Equal));
                }
                if a.buffer.data.is_empty() || b.buffer.data.is_empty() {
                    return Ok(Some(a.buffer.data.len().cmp(&b.buffer.data.len())));
                }
                let left = Arc::as_ptr(a) as usize;
                let right = Arc::as_ptr(b) as usize;
                if !self.buckets.data.is_empty() {
                    let entry = self.buckets.data[self.bucket(left, right)];
                    if entry != 0 {
                        return Ok(self.memo.data[entry - 1].order);
                    }
                }
                let a = &a.buffer.data;
                let b = &b.buffer.data;
                let mut order = Some(a.len().cmp(&b.len()));
                for (a, b) in a.iter().zip(b) {
                    let cmp = self.order(ctx, a, b, depth + 1)?;
                    if cmp != Some(Ordering::Equal) {
                        order = cmp;
                        break;
                    }
                }
                let memo = Memo { left, right, order };
                // Completed pairs bound repeated traversal of shared immutable arrays.
                // Operands must remain alive until clear; key extrema clear before each comparison.
                self.remember(ctx, memo)?;
                Ok(order)
            }
            (Kind::Nil, Kind::Nil) => Ok(Some(Ordering::Equal)),
            (Kind::Bool(a), Kind::Bool(b)) => Ok(Some(a.cmp(b))),
            (Kind::Int(_) | Kind::Float(_), Kind::Int(_) | Kind::Float(_))
            | (Kind::Bytes(_), Kind::Bytes(_))
            | (Kind::Symbol(_), Kind::Symbol(_)) => ops::compare(ctx, a, b),
            _ => Ok(None),
        }
    }

    fn required(&mut self, ctx: &mut CallContext, a: &Value, b: &Value) -> Result<Ordering> {
        self.order(ctx, a, b, 0)?
            .ok_or_else(|| Error::new(ErrorKind::Type, "values are not comparable"))
    }
}

pub(crate) fn spaceship(ctx: &mut CallContext, a: &Value, b: &Value) -> Result<Value> {
    if matches!(a.0, Kind::Bool(_)) || matches!(b.0, Kind::Bool(_)) {
        return Ok(Value::nil());
    }
    let order = Compare::new().order(ctx, a, b, 0)?;
    Ok(order.map_or_else(Value::nil, |order| Value::int(order as i64)))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Method {
    Sort,
    SortBy,
    Min,
    Max,
    Minmax,
    MinBy,
    MaxBy,
}

impl Method {
    fn parse(name: &str) -> Option<Self> {
        Some(match name {
            "sort" => Self::Sort,
            "sort_by" => Self::SortBy,
            "min" => Self::Min,
            "max" => Self::Max,
            "minmax" => Self::Minmax,
            "min_by" => Self::MinBy,
            "max_by" => Self::MaxBy,
            _ => return None,
        })
    }

    fn by(self) -> bool {
        matches!(self, Self::SortBy | Self::MinBy | Self::MaxBy)
    }
}

pub(crate) fn method(name: &str) -> bool {
    Method::parse(name).is_some()
}

pub(crate) struct Driver {
    method: Method,
    receiver: Value,
    values: Buffer<Value>,
    keys: Buffer<Value>,
    sort: Sort,
    compare: Compare,
    position: usize,
    best: usize,
    maximum: usize,
    best_key: Option<Value>,
    block: bool,
    pub waiting: bool,
}

impl Driver {
    pub fn new(
        ctx: &mut CallContext,
        name: &str,
        receiver: &Value,
        args: &[Value],
        block: bool,
    ) -> Result<Option<Self>> {
        let (Some(method), Some(array)) = (Method::parse(name), receiver.as_array()) else {
            return Ok(None);
        };
        ops::arity(args, 0)?;
        if method.by() && !block {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} requires a block"),
            ));
        }
        if block && matches!(method, Method::Min | Method::Max | Method::Minmax) {
            return Err(Error::new(
                ErrorKind::Argument,
                format!("{name} does not accept a block"),
            ));
        }
        let mut values = Buffer::empty();
        let mut keys = Buffer::empty();
        if matches!(method, Method::Sort | Method::SortBy) {
            values.extend(ctx, array)?;
        }
        if method == Method::SortBy {
            keys = Buffer::with_capacity(ctx, array.len())?;
        }
        Ok(Some(Self {
            method,
            receiver: receiver.clone(),
            values,
            keys,
            sort: Sort::new(array.len()),
            compare: Compare::new(),
            position: 0,
            best: 0,
            maximum: 0,
            best_key: None,
            block,
            waiting: false,
        }))
    }

    pub fn advance(&mut self, ctx: &mut CallContext, returned: Option<Value>) -> Result<Progress> {
        self.waiting = false;
        let array = self.receiver.as_array().unwrap();
        let mut comparison = None;
        if let Some(value) = returned {
            ctx.charge(1)?;
            if self.method == Method::Sort {
                comparison = Some(match value.0 {
                    Kind::Int(n) => n.cmp(&0),
                    Kind::Float(n) => n.partial_cmp(&0.0).unwrap_or(Ordering::Equal),
                    _ => {
                        return Err(Error::new(
                            ErrorKind::Argument,
                            "sort comparator must be numeric",
                        ));
                    }
                });
            } else if self.method == Method::SortBy {
                self.keys.push(ctx, value)?;
            } else {
                self.compare.clear();
                let improves = if let Some(best) = &self.best_key {
                    self.compare.required(ctx, &value, best)?
                        == if self.method == Method::MinBy {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                } else {
                    true
                };
                if improves {
                    self.best = self.position - 1;
                    self.best_key = Some(value);
                }
            }
        }
        if self.method.by() && self.position < array.len() {
            ctx.charge(1)?;
            let value = array[self.position].clone();
            self.position += 1;
            self.waiting = true;
            return Ok(Progress::Yield([value, Value::nil(), Value::nil()], 1));
        }
        if matches!(self.method, Method::Sort | Method::SortBy) {
            loop {
                match self.sort.advance(ctx, comparison.take())? {
                    Action::Compare(a, b) => {
                        if self.method == Method::Sort && self.block {
                            self.waiting = true;
                            return Ok(Progress::Yield(
                                [
                                    self.values.data[a].clone(),
                                    self.values.data[b].clone(),
                                    Value::nil(),
                                ],
                                2,
                            ));
                        }
                        let keys = if self.method == Method::SortBy {
                            &self.keys.data
                        } else {
                            &self.values.data
                        };
                        comparison = Some(self.compare.required(ctx, &keys[a], &keys[b])?);
                    }
                    Action::Swap(a, b) => {
                        self.values.data.swap(a, b);
                        if self.method == Method::SortBy {
                            self.keys.data.swap(a, b);
                        }
                    }
                    Action::Done => {
                        let values = std::mem::replace(&mut self.values, Buffer::empty());
                        return Ok(Progress::Done(Value::from_array(ctx, values)?));
                    }
                }
            }
        }
        if !self.method.by() {
            for (index, item) in array.iter().enumerate().skip(1) {
                let order = self.compare.required(ctx, item, &array[self.best])?;
                if order
                    == if self.method == Method::Max {
                        Ordering::Greater
                    } else {
                        Ordering::Less
                    }
                {
                    self.best = index;
                }
                if self.method == Method::Minmax
                    && self.compare.required(ctx, item, &array[self.maximum])? == Ordering::Greater
                {
                    self.maximum = index;
                }
            }
        }
        let best = array.get(self.best).cloned().unwrap_or_default();
        if self.method == Method::Minmax {
            let maximum = array.get(self.maximum).cloned().unwrap_or_default();
            let mut values = Buffer::empty();
            values.extend(ctx, &[best, maximum])?;
            return Ok(Progress::Done(Value::from_array(ctx, values)?));
        }
        Ok(Progress::Done(best))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn comparison_cache_survives_collisions_eviction_and_reset() {
        let colliding: Vec<_> = (1..1_000_000)
            .filter(|left| Compare::hash(*left, 7) & 511 == 511)
            .take(640)
            .collect();
        assert_eq!(colliding.len(), 640);
        let mut counters = Vec::new();
        for keys in [colliding, (1..=640).collect()] {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut compare = Compare::new();
            let order = |i| match i % 4 {
                0 => None,
                1 => Some(Ordering::Less),
                2 => Some(Ordering::Equal),
                _ => Some(Ordering::Greater),
            };
            for (i, left) in keys.iter().copied().enumerate() {
                compare
                    .remember(
                        &mut ctx,
                        Memo {
                            left,
                            right: 7,
                            order: order(i),
                        },
                    )
                    .unwrap();
                for (j, key) in keys
                    .iter()
                    .copied()
                    .enumerate()
                    .take(i + 1)
                    .skip(i.saturating_sub(255))
                {
                    let entry = compare.buckets.data[compare.bucket(key, 7)];
                    assert_ne!(entry, 0, "entry {j} after insertion {i}");
                    assert_eq!(
                        compare.memo.data[entry - 1].order,
                        order(j),
                        "entry {j} after insertion {i}"
                    );
                }
                if i >= 256 {
                    assert_eq!(compare.buckets.data[compare.bucket(keys[i - 256], 7)], 0);
                }
            }
            compare.clear();
            for key in keys {
                assert_eq!(compare.buckets.data[compare.bucket(key, 7)], 0);
            }
            compare
                .remember(
                    &mut ctx,
                    Memo {
                        left: 1,
                        right: 7,
                        order: None,
                    },
                )
                .unwrap();
            let entry = compare.buckets.data[compare.bucket(1, 7)];
            assert_ne!(entry, 0);
            assert_eq!(compare.memo.data[entry - 1].order, None);
            drop(compare);
            let stats = ctx.stats();
            assert_eq!(stats.retained_memory_bytes, 0);
            counters.push((stats.steps, stats.peak_memory_bytes));
        }
        assert_eq!(
            counters[0], counters[1],
            "quota counters must not depend on heap addresses"
        );
    }
}
