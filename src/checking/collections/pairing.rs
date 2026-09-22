//! Array members that regroup elements by position: `zip` and `transpose`.
//!
//! `zip` validates every argument before reading the receiver and pads
//! missing positions with `nil`. `transpose` takes its column count from the
//! first row and rejects rows of any other length. Literal tuples keep exact
//! rows and columns; other arrays keep their element facts per position.

use super::{outcome, unsupported};
use crate::{
    CallContext, Result,
    budget::{Buffer, MAX_VALUE_DEPTH},
    checking::{
        facts::{Atom, Fact, Facts, Node},
        scalar::Operation,
    },
};

/// The array alternatives of one `transpose` row.
struct Row {
    /// Tuple widths and general array elements, by alternative.
    arms: Buffer<Fact>,
    /// The single tuple width shared by every alternative, if any.
    width: Option<usize>,
    /// Whether some alternative has an unknown length.
    general: bool,
}

impl Facts {
    /// Models `array.zip(*arrays)` for one receiver arm. Each argument must be
    /// an array; a row pairs each receiver element with the argument elements
    /// at the same position, or `nil` beyond an argument's end. Wrapping rows
    /// can reach the runtime's value depth limit, which the caller reports.
    pub(super) fn zip_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let mut columns = Buffer::with_capacity(ctx, args.len())?;
        let mut possible = true;
        for &arg in args {
            ctx.charge(1)?;
            let mut arrays = Buffer::empty();
            for i in 0..self.arm_count(arg) {
                ctx.charge(1)?;
                let arm = self.arm(arg, i);
                match self.node(arm) {
                    Node::Atom(Atom::Never) => (),
                    Node::Array(_) | Node::Tuple(_) => arrays.push(ctx, arm)?,
                    Node::Atom(Atom::Unknown | Atom::Any) => {
                        result.throws = true;
                        arrays.push(ctx, Atom::Unknown.fact())?;
                    }
                    Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                        result.unsupported = true;
                    }
                    _ => result.rejected = true,
                }
            }
            possible &= !arrays.data.is_empty();
            let column = self.union(ctx, &arrays.data)?;
            columns.data.push(column);
        }
        if !possible {
            return Ok(result);
        }
        // A result nested beyond the value limit is a limit error with no
        // successful path, which an operation summary cannot describe.
        let mut deepest = self.depth(receiver);
        for &column in &columns.data {
            deepest = deepest.max(self.depth(column));
        }
        if deepest.saturating_add(1) > MAX_VALUE_DEPTH {
            return Ok(Operation {
                rejected: result.rejected,
                ..unsupported()
            });
        }
        result.value = match self.node(receiver) {
            Node::Tuple(items) => {
                let length = items.data.len();
                let mut rows = Buffer::with_capacity(ctx, length)?;
                for index in 0..length {
                    ctx.charge(1)?;
                    let Node::Tuple(items) = self.node(receiver) else {
                        unreachable!()
                    };
                    let mut row = Buffer::with_capacity(ctx, columns.data.len() + 1)?;
                    row.data.push(items.data[index]);
                    for &column in &columns.data {
                        let value = self.zip_position(ctx, column, Some(index))?;
                        row.push(ctx, value)?;
                    }
                    let row = self.tuple(ctx, &row.data)?;
                    rows.data.push(row);
                }
                self.tuple(ctx, &rows.data)?
            }
            Node::Array(element) => {
                let element = *element;
                if element == Atom::Never.fact() {
                    self.tuple(ctx, &[])?
                } else {
                    let mut row = Buffer::with_capacity(ctx, columns.data.len() + 1)?;
                    row.data.push(element);
                    for &column in &columns.data {
                        let value = self.zip_position(ctx, column, None)?;
                        row.push(ctx, value)?;
                    }
                    let row = self.tuple(ctx, &row.data)?;
                    self.array(ctx, row)?
                }
            }
            _ => unreachable!(),
        };
        Ok(result)
    }

    /// The value a zip row reads from one argument at `index`, or at an
    /// unknown position when the receiver length is unknown.
    fn zip_position(
        &mut self,
        ctx: &mut CallContext,
        column: Fact,
        index: Option<usize>,
    ) -> Result<Fact> {
        let nil = Atom::Nil.fact();
        let mut values = Buffer::empty();
        for i in 0..self.arm_count(column) {
            ctx.charge(1)?;
            let arm = self.arm(column, i);
            match (self.node(arm), index) {
                (Node::Tuple(items), Some(index)) => {
                    values.push(ctx, items.data.get(index).copied().unwrap_or(nil))?;
                }
                (Node::Tuple(items), None) => {
                    values.extend(ctx, &items.data)?;
                    values.push(ctx, nil)?;
                }
                (Node::Array(element), _) => {
                    let element = *element;
                    values.push(ctx, element)?;
                    values.push(ctx, nil)?;
                }
                _ => values.push(ctx, arm)?,
            }
        }
        self.union(ctx, &values.data)
    }

    /// Describes the array alternatives of one row, recording invalid and
    /// gradual alternatives on `result`.
    fn transpose_row(
        &mut self,
        ctx: &mut CallContext,
        row: Fact,
        result: &mut Operation,
    ) -> Result<Row> {
        let mut view = Row {
            arms: Buffer::empty(),
            width: None,
            general: false,
        };
        let mut widths = None;
        for i in 0..self.arm_count(row) {
            ctx.charge(1)?;
            let arm = self.arm(row, i);
            match self.node(arm) {
                Node::Atom(Atom::Never) => continue,
                Node::Tuple(items) => {
                    let width = items.data.len();
                    widths = match widths {
                        None => Some(Some(width)),
                        Some(Some(known)) if known == width => Some(Some(known)),
                        Some(_) => Some(None),
                    };
                }
                Node::Array(_) => view.general = true,
                Node::Atom(Atom::Unknown | Atom::Any) => {
                    view.general = true;
                    result.throws = true;
                }
                Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                    result.unsupported = true;
                    continue;
                }
                _ => {
                    result.rejected = true;
                    continue;
                }
            }
            view.arms.push(ctx, arm)?;
        }
        view.width = widths.flatten().filter(|_| !view.general);
        Ok(view)
    }

    /// The union of every value one row alternative stores at `index`, or
    /// anywhere when `index` is `None`.
    fn row_values(
        &mut self,
        ctx: &mut CallContext,
        row: &Row,
        index: Option<usize>,
    ) -> Result<Fact> {
        let mut values = Buffer::empty();
        for &arm in &row.arms.data {
            ctx.charge(1)?;
            match (self.node(arm), index) {
                (Node::Tuple(items), Some(index)) => values.push(ctx, items.data[index])?,
                (Node::Tuple(items), None) => values.extend(ctx, &items.data)?,
                (Node::Array(element), _) => {
                    let element = *element;
                    values.push(ctx, element)?;
                }
                _ => values.push(ctx, Atom::Unknown.fact())?,
            }
        }
        self.union(ctx, &values.data)
    }

    /// Models `array.transpose` for one receiver arm after arity has been
    /// checked. Every row must be an array with the first row's length.
    pub(super) fn transpose_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let mut rows = Buffer::empty();
        let literal = match self.node(receiver) {
            Node::Tuple(items) => {
                let mut items_copy = Buffer::with_capacity(ctx, items.data.len())?;
                items_copy.extend(ctx, &items.data)?;
                for row in items_copy.data {
                    let view = self.transpose_row(ctx, row, &mut result)?;
                    rows.push(ctx, view)?;
                }
                true
            }
            Node::Array(element) => {
                let element = *element;
                if element == Atom::Never.fact() {
                    result.value = self.tuple(ctx, &[])?;
                    return Ok(result);
                }
                let view = self.transpose_row(ctx, element, &mut result)?;
                rows.push(ctx, view)?;
                false
            }
            _ => unreachable!(),
        };
        for row in &rows.data {
            if row.arms.data.is_empty() {
                // A literal row that is certainly not an array always fails;
                // a general array of such rows succeeds only when empty.
                if !literal {
                    result.value = self.tuple(ctx, &[])?;
                }
                return Ok(result);
            }
        }
        if rows.data.is_empty() {
            result.value = self.tuple(ctx, &[])?;
            return Ok(result);
        }
        let mut width = rows.data[0].width;
        for row in &rows.data {
            ctx.charge(1)?;
            if row.width != width {
                width = None;
            }
        }
        if let Some(width) = width {
            let mut columns = Buffer::with_capacity(ctx, width)?;
            for index in 0..width {
                ctx.charge(1)?;
                let mut column = Buffer::with_capacity(ctx, rows.data.len())?;
                for row in &rows.data {
                    let value = self.row_values(ctx, row, Some(index))?;
                    column.push(ctx, value)?;
                }
                let column = if literal {
                    self.tuple(ctx, &column.data)?
                } else {
                    // One row description stands for every row of a general array.
                    self.array(ctx, column.data[0])?
                };
                columns.data.push(column);
            }
            let columns = self.tuple(ctx, &columns.data)?;
            result.value = if literal || width == 0 {
                columns
            } else {
                let empty = self.tuple(ctx, &[])?;
                self.union(ctx, &[empty, columns])?
            };
            return Ok(result);
        }
        // Row lengths may differ; a single-width literal mismatch always fails.
        let mut known = true;
        let mut widths = None;
        let mut mismatch = false;
        for row in &rows.data {
            ctx.charge(1)?;
            match row.width {
                Some(width) => {
                    mismatch |= widths.is_some_and(|first| first != width);
                    widths.get_or_insert(width);
                }
                None => known = false,
            }
        }
        if literal && known && mismatch {
            result.rejected = true;
            return Ok(result);
        }
        // A single literal row fixes the column count itself.
        if !literal || rows.data.len() > 1 {
            result.throws = true;
        }
        let mut column = Buffer::with_capacity(ctx, rows.data.len())?;
        for row in &rows.data {
            let value = self.row_values(ctx, row, None)?;
            column.push(ctx, value)?;
        }
        let mut empty = false;
        for &value in &column.data {
            empty |= value == Atom::Never.fact();
        }
        result.value = if empty {
            self.tuple(ctx, &[])?
        } else {
            let column = if literal {
                self.tuple(ctx, &column.data)?
            } else {
                self.array(ctx, column.data[0])?
            };
            self.array(ctx, column)?
        };
        Ok(result)
    }
}
