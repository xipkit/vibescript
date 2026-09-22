//! Array set operators: `left - right` and `left & right`.
//!
//! The runtime requires two arrays and compares elements with its set
//! membership policy: root numeric kinds must match and NaN matches itself.
//! Literal tuples keep their order and exact length when every membership
//! decision is known; otherwise the result keeps the receiver's elements.

use super::outcome;
use crate::{
    CallContext, Result,
    budget::Buffer,
    checking::{
        facts::{Atom, Fact, Facts, Node},
        scalar::Operation,
    },
};

/// Whether an element is known to be in, known to be outside, or possibly in a set.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Membership {
    Yes,
    No,
    Maybe,
}

impl Membership {
    fn join(self, other: Self) -> Self {
        if self == other { self } else { Self::Maybe }
    }
}

impl Facts {
    /// Models one array arm on the left of `-` or `&`. `right` is the whole
    /// right operand; each of its arms must be an array. Unknown right arms
    /// keep a possible type failure, which the caller reports as a gradual
    /// operand, and the result keeps the left elements.
    pub(in crate::checking) fn array_set(
        &mut self,
        ctx: &mut CallContext,
        op: &str,
        left: Fact,
        right: Fact,
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let mut arrays = Buffer::empty();
        let mut gradual = false;
        for i in 0..self.arm_count(right) {
            ctx.charge(1)?;
            let arm = self.arm(right, i);
            match self.node(arm) {
                Node::Atom(Atom::Never) => (),
                Node::Array(_) | Node::Tuple(_) => arrays.push(ctx, arm)?,
                Node::Atom(Atom::Unknown | Atom::Any) => gradual = true,
                Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                    result.unsupported = true;
                }
                _ => result.rejected = true,
            }
        }
        if arrays.data.is_empty() && !gradual {
            return Ok(result);
        }
        let right = self.union(ctx, &arrays.data)?;
        let general = |facts: &mut Self, ctx: &mut CallContext| -> Result<Fact> {
            let element = facts.elements(ctx, left)?;
            if element == Atom::Never.fact() {
                facts.tuple(ctx, &[])
            } else {
                facts.array(ctx, element)
            }
        };
        let value = if gradual {
            general(self, ctx)?
        } else if let Node::Tuple(items) = self.node(left) {
            let mut items_copy = Buffer::empty();
            items_copy.extend(ctx, &items.data)?;
            match self.exact_set(ctx, op, &items_copy.data, right)? {
                Some(value) => value,
                None => general(self, ctx)?,
            }
        } else if op == "&" && self.known_empty(ctx, right)? {
            self.tuple(ctx, &[])?
        } else {
            general(self, ctx)?
        };
        result.value = value;
        Ok(result)
    }

    /// Computes the exact tuple result when every membership and duplicate
    /// decision is known, or `None` when the result length is uncertain.
    fn exact_set(
        &mut self,
        ctx: &mut CallContext,
        op: &str,
        items: &[Fact],
        right: Fact,
    ) -> Result<Option<Fact>> {
        if op == "&" && self.known_empty(ctx, right)? {
            return Ok(Some(self.tuple(ctx, &[])?));
        }
        let mut kept: Buffer<Fact> = Buffer::empty();
        'item: for &item in items {
            ctx.charge(1)?;
            let membership = self.membership(ctx, item, right)?;
            let keep = match (op, membership) {
                (_, Membership::Maybe) => return Ok(None),
                ("-", Membership::Yes) | ("&", Membership::No) => false,
                _ => true,
            };
            if !keep {
                continue;
            }
            if op == "&" {
                // Intersection also drops later duplicates of kept elements.
                for index in 0..kept.data.len() {
                    ctx.charge(1)?;
                    let earlier = kept.data[index];
                    let equal = self.set_equal(ctx, earlier, item)?;
                    match self.node(equal) {
                        Node::Boolean(true) => continue 'item,
                        Node::Boolean(false) => (),
                        _ => return Ok(None),
                    }
                }
            }
            kept.push(ctx, item)?;
        }
        Ok(Some(self.tuple(ctx, &kept.data)?))
    }

    fn known_empty(&mut self, ctx: &mut CallContext, arrays: Fact) -> Result<bool> {
        for i in 0..self.arm_count(arrays) {
            ctx.charge(1)?;
            let arm = self.arm(arrays, i);
            if self.elements(ctx, arm)? != Atom::Never.fact() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn membership(
        &mut self,
        ctx: &mut CallContext,
        item: Fact,
        arrays: Fact,
    ) -> Result<Membership> {
        let mut result = None;
        for i in 0..self.arm_count(arrays) {
            ctx.charge(1)?;
            let arm = self.arm(arrays, i);
            let next = match self.node(arm) {
                Node::Tuple(values) => {
                    let mut values_copy = Buffer::empty();
                    values_copy.extend(ctx, &values.data)?;
                    let mut found = Membership::No;
                    for &value in &values_copy.data {
                        ctx.charge(1)?;
                        let equal = self.set_equal(ctx, value, item)?;
                        match self.node(equal) {
                            Node::Boolean(true) => {
                                found = Membership::Yes;
                                break;
                            }
                            Node::Boolean(false) => (),
                            _ => found = Membership::Maybe,
                        }
                    }
                    found
                }
                Node::Array(element) => {
                    let element = *element;
                    if element == Atom::Never.fact() {
                        Membership::No
                    } else {
                        let equal = self.set_equal(ctx, element, item)?;
                        if matches!(self.node(equal), Node::Boolean(false)) {
                            Membership::No
                        } else {
                            Membership::Maybe
                        }
                    }
                }
                _ => unreachable!(),
            };
            result = Some(result.map_or(next, |current: Membership| current.join(next)));
        }
        Ok(result.unwrap_or(Membership::No))
    }
}
