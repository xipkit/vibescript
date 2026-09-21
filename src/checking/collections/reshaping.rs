//! Block-free array reshaping members: `compact` and the sized `chunk(n)`.
//!
//! These are separate overloads from the block-driven `chunk` the flow walker
//! models; the runtime routes them through `collections::array_method`, which
//! accepts no block and validates only positional arguments.

use super::{outcome, rejected, unsupported};
use crate::{
    CallContext, Result,
    budget::{Buffer, MAX_VALUE_DEPTH},
    checking::{
        facts::{Atom, Fact, Facts, Node},
        integers::Bounds,
        scalar::{Operation, Test},
    },
};

impl Facts {
    /// Models `array.compact`: every element that is exactly `nil` is dropped
    /// and `nil` is removed from the remaining element domains. The result is
    /// an exact tuple only when no kept element may still be `nil`; otherwise
    /// the cardinality is unknown and the result is a general array.
    pub(super) fn compact_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
    ) -> Result<Operation> {
        let nil = Atom::Nil.fact();
        match self.node(receiver) {
            Node::Tuple(values) => {
                let mut items = Buffer::empty();
                items.extend(ctx, &values.data)?;
                let mut kept = Buffer::empty();
                let mut exact = true;
                for &value in &items.data {
                    ctx.charge(1)?;
                    if value == nil {
                        continue;
                    }
                    let present = self.filter(ctx, value, Test::Nil, false)?;
                    exact &= self.filter(ctx, value, Test::Nil, true)? == Atom::Never.fact();
                    kept.push(ctx, present)?;
                }
                let value = if exact {
                    self.tuple(ctx, &kept.data)?
                } else {
                    let element = self.union(ctx, &kept.data)?;
                    self.array(ctx, element)?
                };
                Ok(outcome(value))
            }
            Node::Array(element) => {
                let element = self.filter(ctx, *element, Test::Nil, false)?;
                let value = if element == Atom::Never.fact() {
                    self.tuple(ctx, &[])?
                } else {
                    self.array(ctx, element)?
                };
                Ok(outcome(value))
            }
            _ => unreachable!(),
        }
    }

    /// Models `array.chunk(size)` without a block. The runtime accepts only a
    /// machine integer that is positive: floats, big integers and every other
    /// value fail before the receiver is read, so a broad integer domain keeps
    /// a possible failure while a literal or fully bounded positive size does
    /// not. Each valid arm of `size` contributes its own result shape.
    pub(super) fn chunk_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        size: Fact,
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        for i in 0..self.arm_count(size) {
            ctx.charge(1)?;
            let arm = self.arm(size, i);
            let next = match self.node(arm) {
                Node::Atom(Atom::Never) => continue,
                Node::Integer(_) | Node::IntegerBounds(_) | Node::Atom(Atom::Int) => {
                    let bounds = self.integer_bounds(arm).unwrap();
                    if bounds.max.is_some_and(|max| max <= 0) {
                        rejected()
                    } else {
                        // A domain open above admits big integers, which the
                        // runtime rejects; one reaching zero may be non-positive.
                        let safe = bounds.min.is_some_and(|min| min >= 1) && bounds.max.is_some();
                        let positive = Bounds {
                            min: Some(bounds.min.map_or(1, |min| min.max(1))),
                            max: bounds.max,
                        };
                        Operation {
                            throws: !safe,
                            ..self.chunks(ctx, receiver, positive)?
                        }
                    }
                }
                Node::Atom(Atom::Unknown | Atom::Any) => Operation {
                    throws: true,
                    ..self.chunks(
                        ctx,
                        receiver,
                        Bounds {
                            min: Some(1),
                            max: None,
                        },
                    )?
                },
                Node::Array(_)
                | Node::Tuple(_)
                | Node::Hash(..)
                | Node::Shape(..)
                | Node::Protected(..) => rejected(),
                _ if self.atom(arm).is_some() => rejected(),
                _ => unsupported(),
            };
            self.merge_operation(ctx, &mut result, next)?;
        }
        Ok(result)
    }

    /// Builds the chunked result for a positive size domain. A known length
    /// that fits in one chunk, or an exact size, yields exact tuples; other
    /// sizes alias the receiver's elements two levels deep. Wrapping a
    /// receiver already at the value depth limit always fails at runtime, and
    /// the operation summary has no limit channel, so it stays incomplete.
    fn chunks(&mut self, ctx: &mut CallContext, receiver: Fact, size: Bounds) -> Result<Operation> {
        ctx.charge(1)?;
        if self.depth(receiver).saturating_add(1) > MAX_VALUE_DEPTH {
            return Ok(unsupported());
        }
        let exact = size.min.filter(|_| size.min == size.max);
        let values = match self.node(receiver) {
            Node::Tuple(values) => {
                let mut items = Buffer::empty();
                items.extend(ctx, &values.data)?;
                items
            }
            Node::Array(element) => {
                if *element == Atom::Never.fact() {
                    return Ok(outcome(self.tuple(ctx, &[])?));
                }
                let row = self.array(ctx, *element)?;
                return Ok(outcome(self.array(ctx, row)?));
            }
            _ => unreachable!(),
        };
        let length = values.data.len();
        if length == 0 {
            return Ok(outcome(self.tuple(ctx, &[])?));
        }
        let single = size
            .min
            .is_some_and(|min| u64::try_from(min).is_ok_and(|min| min >= length as u64));
        if single {
            let row = self.tuple(ctx, &values.data)?;
            return Ok(outcome(self.tuple(ctx, &[row])?));
        }
        let Some(size) = exact else {
            let element = self.union(ctx, &values.data)?;
            let row = self.array(ctx, element)?;
            return Ok(outcome(self.array(ctx, row)?));
        };
        let size = usize::try_from(size).unwrap_or(usize::MAX).max(1);
        let mut parts = Buffer::with_capacity(ctx, length.div_ceil(size))?;
        let mut start = 0;
        while start < length {
            ctx.charge(1)?;
            let end = start + size.min(length - start);
            let part = self.tuple(ctx, &values.data[start..end])?;
            parts.push(ctx, part)?;
            start = end;
        }
        Ok(outcome(self.tuple(ctx, &parts.data)?))
    }
}
