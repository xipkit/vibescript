//! `range.to_a`, which materializes an integer range in iteration order.

use super::{EXACT, outcome, rejected, unsupported};
use crate::{
    CallContext, Result,
    budget::Buffer,
    checking::{
        facts::{Atom, Fact, Facts, Node},
        integers::Bounds,
        scalar::Operation,
    },
};

impl Facts {
    /// Models `range.to_a` for one range arm. Open ranges cannot be
    /// iterated. Literal ranges produce their exact elements, ascending or
    /// descending, up to [`EXACT`] values and bounded integers beyond that.
    /// A general range may be open or exceed the runtime's size guard, which
    /// is a limit error reported by the caller.
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
            return Ok(unsupported());
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
