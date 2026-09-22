//! Range members that read or materialize integer ranges: `first`, `last`,
//! `size`, `include?`, `cover?`, `member?`, `exclude_end?` and `to_a`.
//!
//! Literal ranges keep their endpoints, so these members fold to exact
//! integers, booleans and element tuples. A general range may be open, and
//! may exceed the runtime's size guards, so it keeps each member's possible
//! runtime failure beside a general result.

use super::{EXACT, outcome, rejected};
use crate::{
    CallContext, Result, Value,
    budget::Buffer,
    checking::{
        facts::{Atom, Fact, Facts, Node},
        integers::Bounds,
        scalar::Operation,
    },
};

/// The endpoints of a literal range fact.
#[derive(Clone, Copy)]
struct Literal {
    start: Option<i64>,
    end: Option<i64>,
    exclusive: bool,
}

impl Literal {
    fn length(self) -> Option<i128> {
        Some((i128::from(self.end?) - i128::from(self.start?)).abs() + i128::from(!self.exclusive))
    }

    /// The inclusive integer interval the range contains, with `None` for an
    /// open side, or `None` when it contains no integer.
    fn interval(self) -> Option<(Option<i128>, Option<i128>)> {
        let exclusive = i128::from(self.exclusive);
        let (low, high) = match (self.start, self.end) {
            (Some(start), Some(end)) if start > end => {
                (Some(i128::from(end) + exclusive), Some(i128::from(start)))
            }
            (start, end) => (
                start.map(i128::from),
                end.map(|end| i128::from(end) - exclusive),
            ),
        };
        match (low, high) {
            (Some(low), Some(high)) if low > high => None,
            interval => Some(interval),
        }
    }

    fn contains(self, value: &Value) -> bool {
        crate::range::Range::untracked(self.start, self.end, self.exclusive).contains(value)
    }
}

/// How one alternative of a `first`/`last` count converts to an integer.
enum Count {
    /// A machine integer within these bounds, and whether the conversion
    /// cannot fail because both bounds are known.
    Known(Bounds, bool),
    Gradual,
    Invalid,
    Unsupported,
    Never,
}

impl Facts {
    fn literal_range(&self, receiver: Fact) -> Option<Literal> {
        match self.node(receiver) {
            Node::Range(start, end, exclusive) => Some(Literal {
                start: *start,
                end: *end,
                exclusive: *exclusive,
            }),
            _ => None,
        }
    }

    /// Models the members listed in the module documentation for one range
    /// arm after the caller has checked their arity.
    pub(in crate::checking) fn range_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        name: &str,
        args: &[Fact],
    ) -> Result<Operation> {
        ctx.charge(1)?;
        let literal = self.literal_range(receiver);
        match name {
            "to_a" => self.range_array(ctx, receiver),
            "exclude_end?" => Ok(outcome(match literal {
                Some(range) => self.boolean(ctx, range.exclusive)?,
                None => Atom::Bool.fact(),
            })),
            "size" => match literal.map(Literal::length) {
                // The runtime reports a length beyond 64 bits as an overflow.
                Some(Some(length)) => match i64::try_from(length) {
                    Ok(length) => Ok(outcome(self.integer(ctx, length)?)),
                    Err(_) => Ok(rejected()),
                },
                Some(None) => Ok(rejected()),
                None => {
                    let length = self.integer_range(
                        ctx,
                        Bounds {
                            min: Some(0),
                            max: None,
                        },
                    )?;
                    Ok(Operation {
                        throws: true,
                        ..outcome(length)
                    })
                }
            },
            "include?" | "cover?" | "member?" => self.range_membership(ctx, literal, args[0]),
            "first" | "last" => {
                let count = args.first().copied();
                match literal {
                    Some(range) => self.range_end(ctx, range, name == "last", count),
                    None => self.general_range_end(ctx, count),
                }
            }
            _ => unreachable!(),
        }
    }

    /// Models `include?`, `cover?` and `member?`, which never fail: integers
    /// and floats compare with the endpoints and every other value is outside.
    fn range_membership(
        &mut self,
        ctx: &mut CallContext,
        literal: Option<Literal>,
        value: Fact,
    ) -> Result<Operation> {
        let (mut yes, mut no) = (false, false);
        for i in 0..self.arm_count(value) {
            ctx.charge(1)?;
            let arm = self.arm(value, i);
            let known = match (self.node(arm), literal) {
                (Node::Atom(Atom::Never), _) => continue,
                (Node::Integer(n), Some(range)) => Some(range.contains(&Value::int(*n))),
                (Node::Float(bits), Some(range)) => {
                    Some(range.contains(&Value::float(f64::from_bits(*bits))))
                }
                (Node::IntegerBounds(bounds), Some(range)) => match range.interval() {
                    None => Some(false),
                    Some(interval) => bounded_membership(interval, *bounds),
                },
                (
                    Node::Integer(_)
                    | Node::Float(_)
                    | Node::IntegerBounds(_)
                    | Node::Atom(Atom::Int | Atom::Float | Atom::Unknown | Atom::Any)
                    | Node::Named(_)
                    | Node::Nominal { .. }
                    | Node::Choice(_),
                    _,
                ) => None,
                _ => Some(false),
            };
            yes |= known != Some(false);
            no |= known != Some(true);
        }
        let value = match (yes, no) {
            (true, true) => Atom::Bool.fact(),
            (true, false) => self.boolean(ctx, true)?,
            (false, true) => self.boolean(ctx, false)?,
            (false, false) => Atom::Never.fact(),
        };
        Ok(outcome(value))
    }

    /// Models `first` and `last` on a general range, which may be open on the
    /// side the member reads.
    fn general_range_end(
        &mut self,
        ctx: &mut CallContext,
        count: Option<Fact>,
    ) -> Result<Operation> {
        let Some(count) = count else {
            return Ok(Operation {
                throws: true,
                ..outcome(Atom::Int.fact())
            });
        };
        let mut result = Operation {
            throws: true,
            ..outcome(Atom::Never.fact())
        };
        let mut valid = false;
        for i in 0..self.arm_count(count) {
            ctx.charge(1)?;
            match self.range_count(self.arm(count, i)) {
                Count::Known(bounds, _) if bounds.max.is_some_and(|max| max < 0) => {
                    result.rejected = true;
                }
                Count::Known(..) | Count::Gradual => valid = true,
                Count::Invalid => result.rejected = true,
                Count::Unsupported => result.unsupported = true,
                Count::Never => (),
            }
        }
        if valid {
            result.value = self.array(ctx, Atom::Int.fact())?;
        }
        Ok(result)
    }

    /// Models `first` and `last` on a literal range. Without a count they
    /// return the endpoint itself, even for exclusive or empty ranges. A
    /// count must be a non-negative machine integer and selects that many
    /// elements from the start, or from the end of a bounded range, in
    /// iteration order.
    fn range_end(
        &mut self,
        ctx: &mut CallContext,
        range: Literal,
        last: bool,
        count: Option<Fact>,
    ) -> Result<Operation> {
        let endpoint = if last { range.end } else { range.start };
        let Some(endpoint) = endpoint else {
            return Ok(rejected());
        };
        let Some(count) = count else {
            return Ok(outcome(self.integer(ctx, endpoint)?));
        };
        let mut result = outcome(Atom::Never.fact());
        for i in 0..self.arm_count(count) {
            ctx.charge(1)?;
            let (bounds, exact) = match self.range_count(self.arm(count, i)) {
                Count::Known(bounds, exact) => (bounds, exact),
                Count::Gradual => (Bounds::ALL, false),
                Count::Invalid => {
                    result.rejected = true;
                    continue;
                }
                Count::Unsupported => {
                    result.unsupported = true;
                    continue;
                }
                Count::Never => continue,
            };
            if bounds.max.is_some_and(|max| max < 0) {
                result.rejected = true;
                continue;
            }
            // The count converts before `last` reads the start of the range.
            let Some(start) = range.start else {
                result.rejected = true;
                continue;
            };
            result.throws |= !exact || bounds.min.is_none_or(|min| min < 0);
            let low = bounds.min.map_or(0, |min| min.max(0));
            let next = self.range_window(ctx, range, start, last, low, bounds.max)?;
            result.value = self.union(ctx, &[result.value, next])?;
        }
        Ok(result)
    }

    /// Classifies one count alternative the way the runtime's `require_int`
    /// does: only machine integers convert, so floats always fail and a broad
    /// integer domain may hold a big integer.
    fn range_count(&self, count: Fact) -> Count {
        match self.node(count) {
            Node::Integer(value) => Count::Known(Bounds::point(*value), true),
            Node::IntegerBounds(bounds) => {
                Count::Known(*bounds, bounds.min.is_some() && bounds.max.is_some())
            }
            Node::Atom(Atom::Int) => Count::Known(Bounds::ALL, false),
            Node::Atom(Atom::Unknown | Atom::Any) => Count::Gradual,
            Node::Atom(Atom::Never) => Count::Never,
            Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => Count::Unsupported,
            _ => Count::Invalid,
        }
    }

    /// The elements `first(n)` or `last(n)` select from a range with a known
    /// start for a non-negative count between `low` and `high`, which is
    /// unbounded when `None`.
    fn range_window(
        &mut self,
        ctx: &mut CallContext,
        range: Literal,
        start: i64,
        last: bool,
        low: i64,
        high: Option<i64>,
    ) -> Result<Fact> {
        let (length, step) = match range.end {
            Some(end) => (range.length().unwrap(), if start > end { -1 } else { 1 }),
            // An endless range stops at the largest machine integer.
            None => (i128::from(i64::MAX) - i128::from(start) + 1, 1),
        };
        let (low, high) = (
            i128::from(low).min(length),
            high.map_or(length, |high| i128::from(high).min(length)),
        );
        if high == 0 {
            return self.tuple(ctx, &[]);
        }
        let origin = |count: i128| {
            let skip = if last { length - count } else { 0 };
            i128::from(start) + skip * step
        };
        let element = |value: i128| {
            i64::try_from(value).expect("selected range elements stay within their endpoints")
        };
        if low == high && high <= EXACT as i128 {
            let first = origin(high);
            let mut values = Buffer::with_capacity(ctx, high as usize)?;
            for offset in 0..high {
                ctx.charge(1)?;
                let value = self.integer(ctx, element(first + step * offset))?;
                values.data.push(value);
            }
            return self.tuple(ctx, &values.data);
        }
        // Every possible selection lies within the longest one.
        let first = origin(high);
        let final_element = first + (high - 1) * step;
        let bounds = Bounds {
            min: Some(element(first.min(final_element))),
            max: Some(element(first.max(final_element))),
        };
        let value = self.integer_range(ctx, bounds)?;
        self.array(ctx, value)
    }

    /// Reports whether `to_a` on a range arm can reach the runtime's size
    /// guard, a limit error that an operation summary does not describe.
    pub(in crate::checking) fn range_array_limit(&self, receiver: Fact) -> bool {
        match self.node(receiver) {
            Node::Range(..) => self
                .literal_range(receiver)
                .and_then(Literal::length)
                .is_some_and(|length| length > i128::from(i64::MAX)),
            _ => self.atom(receiver) == Some(Atom::Range),
        }
    }

    /// Models `range.to_a` for one range arm. Open ranges cannot be
    /// iterated. Literal ranges produce their exact elements, ascending or
    /// descending, up to [`EXACT`] values and bounded integers beyond that.
    /// A general range may be open or exceed the runtime's size guard, and a
    /// literal one beyond 64 bits always does; see [`Self::range_array_limit`].
    pub(super) fn range_array(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
    ) -> Result<Operation> {
        let (start, end, exclusive) = match self.node(receiver) {
            Node::Range(Some(start), Some(end), exclusive) => (*start, *end, *exclusive),
            Node::Range(..) => return Ok(rejected()),
            _ => {
                let element = self.array(ctx, Atom::Int.fact())?;
                return Ok(Operation {
                    throws: true,
                    ..outcome(element)
                });
            }
        };
        let length = (i128::from(end) - i128::from(start)).abs() + i128::from(!exclusive);
        if length > i128::from(i64::MAX) {
            return Ok(outcome(Atom::Never.fact()));
        }
        if length == 0 {
            return Ok(outcome(self.tuple(ctx, &[])?));
        }
        let step = if start > end { -1 } else { 1 };
        let last = i128::from(start) + step * (length - 1);
        let last = i64::try_from(last).expect("range elements stay within their endpoints");
        if length > EXACT as i128 {
            let element = self.integer_range(
                ctx,
                Bounds {
                    min: Some(start.min(last)),
                    max: Some(start.max(last)),
                },
            )?;
            return Ok(outcome(self.array(ctx, element)?));
        }
        let mut values = Buffer::with_capacity(ctx, length as usize)?;
        for offset in 0..length {
            ctx.charge(1)?;
            let value = i64::try_from(i128::from(start) + step * offset)
                .expect("range elements stay within their endpoints");
            let value = self.integer(ctx, value)?;
            values.data.push(value);
        }
        Ok(outcome(self.tuple(ctx, &values.data)?))
    }
}

/// Whether every integer in `bounds` is inside the inclusive `interval`
/// (`Some(true)`), none is (`Some(false)`), or it depends on the value.
fn bounded_membership(interval: (Option<i128>, Option<i128>), bounds: Bounds) -> Option<bool> {
    let (low, high) = interval;
    let (min, max) = (bounds.min.map(i128::from), bounds.max.map(i128::from));
    let inside = low.is_none_or(|low| min.is_some_and(|min| low <= min))
        && high.is_none_or(|high| max.is_some_and(|max| max <= high));
    let outside = low.is_some_and(|low| max.is_some_and(|max| max < low))
        || high.is_some_and(|high| min.is_some_and(|min| min > high));
    if inside {
        Some(true)
    } else if outside {
        Some(false)
    } else {
        None
    }
}
