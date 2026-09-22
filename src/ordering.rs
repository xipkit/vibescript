use crate::{
    CallContext, Error, ErrorKind, Result, Value,
    budget::{Buffer, MAX_VALUE_DEPTH},
    iteration::Progress,
    ops,
    pairs::Pairs,
    sort::{Action, Sort},
    value::Kind,
};
use std::{cmp::Ordering, sync::Arc};

struct Compare {
    /// Orders of shared array pairs finished during the current comparison.
    pairs: Pairs<Option<Ordering>>,
}

/// One suspended array comparison; borrows the operands only, so unwinding
/// the walk never runs recursive drop glue. `shared` holds the storage
/// addresses of a pair that other paths may reach again.
struct Frame<'a> {
    shared: Option<(usize, usize)>,
    a: &'a [Value],
    b: &'a [Value],
    index: usize,
    order: Option<Ordering>,
}

impl Frame<'_> {
    /// Records a decisive element result and skips the remaining elements.
    fn finish(&mut self, order: Option<Ordering>) {
        self.order = order;
        self.index = self.a.len().min(self.b.len());
    }
}

enum Step<'a> {
    Done(Option<Ordering>),
    Enter(Frame<'a>),
}

impl Compare {
    fn new() -> Self {
        Self {
            pairs: Pairs::new(),
        }
    }

    fn order<'a>(
        &mut self,
        ctx: &mut CallContext,
        a: &'a Value,
        b: &'a Value,
        depth: usize,
    ) -> Result<Option<Ordering>> {
        // Remembered addresses may belong to values released since the last
        // comparison, so each comparison starts from an empty memo.
        self.pairs.clear(ctx)?;
        let mut current = match self.step(ctx, a, b, depth)? {
            Step::Done(order) => return Ok(order),
            Step::Enter(mut frame) => {
                // The operands themselves cannot be reached again.
                frame.shared = None;
                frame
            }
        };
        // Suspended ancestors of `current`; charged per push, released on any exit.
        let mut parents: Buffer<Frame<'a>> = Buffer::empty();
        loop {
            let i = current.index;
            if i < current.a.len().min(current.b.len()) {
                current.index += 1;
                let (a, b): (&'a [Value], &'a [Value]) = (current.a, current.b);
                match self.step(ctx, &a[i], &b[i], depth + parents.data.len() + 1)? {
                    Step::Done(cmp) => {
                        if cmp != Some(Ordering::Equal) {
                            current.finish(cmp);
                        }
                    }
                    Step::Enter(frame) => {
                        let suspended = std::mem::replace(&mut current, frame);
                        parents.push(ctx, suspended)?;
                    }
                }
                continue;
            }
            let order = current.order;
            // Completed shared pairs bound repeated traversal of immutable arrays.
            if let Some((left, right)) = current.shared {
                self.pairs.insert(ctx, left, right, order)?;
            }
            match parents.data.pop() {
                Some(parent) => {
                    current = parent;
                    if order != Some(Ordering::Equal) {
                        current.finish(order);
                    }
                }
                None => return Ok(order),
            }
        }
    }

    /// Compares one pair without descending; array pairs that need element
    /// comparison return a frame positioned at their first element.
    fn step<'a>(
        &mut self,
        ctx: &mut CallContext,
        a: &'a Value,
        b: &'a Value,
        depth: usize,
    ) -> Result<Step<'a>> {
        ctx.charge(1)?;
        if depth > MAX_VALUE_DEPTH {
            return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
        }
        let reach = depth + a.depth().max(b.depth());
        let order = match (&a.0, &b.0) {
            (Kind::Array(a), Kind::Array(b)) => {
                if Arc::ptr_eq(a, b) {
                    return Ok(Step::Done(Some(Ordering::Equal)));
                }
                if a.buffer.data.is_empty() || b.buffer.data.is_empty() {
                    return Ok(Step::Done(Some(
                        a.buffer.data.len().cmp(&b.buffer.data.len()),
                    )));
                }
                let shared = ops::shared_pair(a, b);
                // A remembered order stands in for its walk only where that
                // walk could not reach the nesting limit from this depth.
                if let Some((left, right)) = shared.filter(|_| reach <= MAX_VALUE_DEPTH) {
                    if let Some(order) = self.pairs.get(left, right) {
                        return Ok(Step::Done(order));
                    }
                }
                return Ok(Step::Enter(Frame {
                    shared,
                    a: &a.buffer.data,
                    b: &b.buffer.data,
                    index: 0,
                    order: Some(a.buffer.data.len().cmp(&b.buffer.data.len())),
                }));
            }
            (Kind::Nil, Kind::Nil) => Some(Ordering::Equal),
            (Kind::Money(a), Kind::Money(b)) => a.order(*b),
            (Kind::Duration(a), Kind::Duration(b)) => Some(crate::duration::order(*a, *b)),
            (Kind::Time(_) | Kind::Zoned(_), Kind::Time(_) | Kind::Zoned(_)) => Some(
                crate::time::stamp(a)
                    .unwrap()
                    .order(crate::time::stamp(b).unwrap()),
            ),
            (Kind::Bool(a), Kind::Bool(b)) => Some(a.cmp(b)),
            (
                Kind::Int(_) | Kind::Big(_) | Kind::Float(_),
                Kind::Int(_) | Kind::Big(_) | Kind::Float(_),
            )
            | (Kind::Bytes(_), Kind::Bytes(_))
            | (Kind::Symbol(_), Kind::Symbol(_)) => ops::compare(ctx, a, b)?,
            _ => None,
        };
        Ok(Step::Done(order))
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
                    Kind::Big(ref n) => {
                        if n.negative {
                            Ordering::Less
                        } else {
                            Ordering::Greater
                        }
                    }
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
    use crate::{
        CallOptions,
        ops::testing::{context, nested, on_small_stack, shared},
    };

    #[test]
    fn shared_pairs_are_remembered_within_one_comparison_only() {
        // Each level repeats its child; without the memo the walk doubles per level.
        let a = shared(60, ints(&[1, 2]));
        let b = shared(60, ints(&[1, 2]));
        let c = shared(60, ints(&[1, 3]));
        let mut ctx = context(Some(2_000), Some(usize::MAX));
        let mut compare = Compare::new();
        assert_eq!(
            compare.order(&mut ctx, &a, &b, 0).unwrap(),
            Some(Ordering::Equal)
        );
        let first = ctx.stats();
        assert!(first.retained_memory_bytes > 0);
        assert_eq!(
            compare.order(&mut ctx, &a, &b, 0).unwrap(),
            Some(Ordering::Equal)
        );
        // Nothing carries over: the repeat costs the same work again.
        let second = ctx.stats();
        assert_eq!(second.steps - first.steps, first.steps);
        assert_eq!(
            compare.order(&mut ctx, &a, &c, 0).unwrap(),
            Some(Ordering::Less)
        );
        drop(compare);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn remembered_orders_do_not_hide_the_nesting_limit() {
        let outcome = on_small_stack(|| {
            let mut ctx = CallContext::new(CallOptions::default());
            let inner = nested(MAX_VALUE_DEPTH - 2, Value::int(1));
            let other = nested(MAX_VALUE_DEPTH - 2, Value::int(1));
            let deep = |value: &Value| Value::array(vec![value.clone(), nested(3, value.clone())]);
            let result = Compare::new().order(&mut ctx, &deep(&inner), &deep(&other), 0);
            let stats = ctx.stats();
            drop(inner);
            drop(other);
            (result, stats)
        });
        let (result, stats) = outcome;
        assert_eq!(result.unwrap_err().kind, ErrorKind::Recursion);
        assert_eq!(stats.retained_memory_bytes, 0);
    }

    fn ints(values: &[i64]) -> Value {
        Value::array(values.iter().copied().map(Value::int).collect())
    }

    #[test]
    fn lexicographic_walk_exits_early() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut compare = Compare::new();
        let a = Value::array(vec![ints(&[1, 2]), ints(&[9])]);
        let b = Value::array(vec![ints(&[1, 3]), ints(&[0])]);
        assert_eq!(
            compare.order(&mut ctx, &a, &b, 0).unwrap(),
            Some(Ordering::Less)
        );
        let early = ctx.stats().steps;
        assert_eq!(
            compare.order(&mut ctx, &b, &a, 0).unwrap(),
            Some(Ordering::Greater)
        );
        let mut ctx = CallContext::new(CallOptions::default());
        let same = Value::array(vec![ints(&[1, 2]), ints(&[9])]);
        assert_eq!(
            Compare::new().order(&mut ctx, &a, &same, 0).unwrap(),
            Some(Ordering::Equal)
        );
        // The early exit never visited the second pair of elements.
        assert!(ctx.stats().steps > early);
        let prefix = Value::array(vec![ints(&[1])]);
        let longer = Value::array(vec![ints(&[1, 2])]);
        assert_eq!(
            Compare::new().order(&mut ctx, &prefix, &longer, 0).unwrap(),
            Some(Ordering::Less)
        );
        let shorter_outer = Value::array(vec![ints(&[1, 2])]);
        let longer_outer = Value::array(vec![ints(&[1, 2]), ints(&[0])]);
        assert_eq!(
            Compare::new()
                .order(&mut ctx, &shorter_outer, &longer_outer, 0)
                .unwrap(),
            Some(Ordering::Less)
        );
        let mixed = Value::array(vec![Value::array(vec![Value::bytes("a")])]);
        let mut compare = Compare::new();
        assert_eq!(compare.order(&mut ctx, &prefix, &mixed, 0).unwrap(), None);
        let nan = Value::array(vec![Value::array(vec![Value::float(f64::NAN)])]);
        assert_eq!(
            Compare::new().order(&mut ctx, &nan, &nan, 0).unwrap(),
            Some(Ordering::Equal)
        );
        assert_eq!(
            Compare::new()
                .order(&mut ctx, &nan, &nan.as_array().unwrap()[0], 0)
                .unwrap(),
            None
        );
        drop(compare);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn nested_ordering_has_exact_step_and_memory_boundaries() {
        let a = Value::array(vec![ints(&[1, 2]), Value::array(vec![ints(&[3])])]);
        let b = Value::array(vec![ints(&[1, 2]), Value::array(vec![ints(&[3])])]);
        let mut ctx = CallContext::new(CallOptions::default());
        assert_eq!(
            Compare::new().order(&mut ctx, &a, &b, 0).unwrap(),
            Some(Ordering::Equal)
        );
        let steps = ctx.stats().steps;
        let mut ctx = context(Some(steps), None);
        assert_eq!(
            Compare::new().order(&mut ctx, &a, &b, 0).unwrap(),
            Some(Ordering::Equal)
        );
        let mut ctx = context(Some(steps - 1), None);
        let mut compare = Compare::new();
        let error = compare.order(&mut ctx, &a, &b, 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Steps);
        drop(compare);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(0).unwrap_err(), error);

        let inner = ints(&[1]);
        let outer = Value::array(vec![inner.clone()]);
        let other = Value::array(vec![ints(&[1])]);
        let frames = 8 * size_of::<Frame>();
        // `inner` is shared with this test, so its completed pair is remembered.
        let memo = 8 * size_of::<(usize, usize, Option<Ordering>)>();
        let mut ctx = context(None, Some(frames + memo));
        let mut compare = Compare::new();
        assert_eq!(
            compare.order(&mut ctx, &outer, &other, 0).unwrap(),
            Some(Ordering::Equal)
        );
        assert_eq!(ctx.stats().peak_memory_bytes, frames + memo);
        drop(compare);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        for limit in [frames + memo - 1, frames - 1] {
            let mut ctx = context(None, Some(limit));
            let mut compare = Compare::new();
            let error = compare.order(&mut ctx, &outer, &other, 0).unwrap_err();
            assert_eq!(error.kind, ErrorKind::Memory);
            if limit < frames {
                assert_eq!(ctx.stats().peak_memory_bytes, 0);
            }
            drop(compare);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            assert_eq!(ctx.charge(0).unwrap_err(), error);
        }
        // Unshared and flat operands store nothing.
        let mut ctx = context(None, Some(frames));
        let alone = Value::array(vec![ints(&[1])]);
        assert_eq!(
            Compare::new().order(&mut ctx, &alone, &other, 0).unwrap(),
            Some(Ordering::Equal)
        );
        assert_eq!(ctx.stats().peak_memory_bytes, frames);
        let mut ctx = context(None, Some(0));
        assert_eq!(
            Compare::new()
                .order(&mut ctx, &ints(&[1, 2]), &ints(&[1, 3]), 0)
                .unwrap(),
            Some(Ordering::Less)
        );
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        drop(inner);
    }

    #[test]
    fn nested_ordering_observes_cancellation_mid_walk() {
        let a = Value::array(vec![ints(&[1, 2, 3, 4]), ints(&[5, 6, 7, 8])]);
        let b = Value::array(vec![ints(&[1, 2, 3, 4]), ints(&[5, 6, 7, 8])]);
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.charge(14).unwrap();
        ctx.cancellation().cancel();
        let mut compare = Compare::new();
        let error = compare.order(&mut ctx, &a, &b, 0).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Cancelled);
        assert_eq!(ctx.stats().steps, 16);
        drop(compare);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err(), error);
    }

    #[test]
    fn shared_graph_ordering_completes_through_the_memo() {
        let levels = MAX_VALUE_DEPTH - 1;
        let a = shared(levels, Value::int(0));
        let b = shared(levels, Value::int(0));
        let c = shared(levels, Value::int(1));
        let mut ctx = context(None, None);
        let mut compare = Compare::new();
        assert_eq!(
            compare.order(&mut ctx, &a, &b, 0).unwrap(),
            Some(Ordering::Equal)
        );
        assert_eq!(
            compare.order(&mut ctx, &a, &c, 0).unwrap(),
            Some(Ordering::Less)
        );
        assert!(ctx.stats().steps <= 16 * levels as u64);
        drop(compare);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        for value in [a, b, c] {
            drop(value);
        }
    }

    #[test]
    fn deep_ordering_walks_to_the_value_limit_on_a_small_stack() {
        let outcome = on_small_stack(|| {
            let mut ctx = CallContext::new(CallOptions::default());
            let values = (
                nested(MAX_VALUE_DEPTH, Value::int(1)),
                nested(MAX_VALUE_DEPTH, Value::int(1)),
                nested(MAX_VALUE_DEPTH, Value::int(2)),
                nested(MAX_VALUE_DEPTH + 1, Value::int(1)),
                nested(MAX_VALUE_DEPTH + 1, Value::int(1)),
            );
            let mut compare = Compare::new();
            let within = (
                compare.order(&mut ctx, &values.0, &values.1, 0),
                compare.order(&mut ctx, &values.0, &values.2, 0),
                compare.order(&mut ctx, &values.2, &values.0, 0),
            );
            let beyond = compare.order(&mut ctx, &values.3, &values.4, 0);
            drop(compare);
            let stats = ctx.stats();
            for value in [values.0, values.1, values.2, values.3, values.4] {
                drop(value);
            }
            (within, beyond, stats)
        });
        let (within, beyond, stats) = outcome;
        assert_eq!(within.0.unwrap(), Some(Ordering::Equal));
        assert_eq!(within.1.unwrap(), Some(Ordering::Less));
        assert_eq!(within.2.unwrap(), Some(Ordering::Greater));
        let error = beyond.unwrap_err();
        assert_eq!(error.kind, ErrorKind::Recursion);
        assert_eq!(error.class(), Some(crate::ErrorClass::Limit));
        assert_eq!(error.message, "value nesting too deep");
        assert_eq!(stats.retained_memory_bytes, 0);
    }
}
