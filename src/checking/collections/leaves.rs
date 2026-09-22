//! Members that walk nested collections: `join`, `inspect` and `flatten`.
//!
//! The runtime walks nested arrays with its own explicit stack. Analysis uses
//! metered work lists over the fact graph, so deep facts never grow the Rust
//! stack; memo tables keep shared facts from being visited repeatedly.

use super::{Count, outcome};
use crate::{
    CallContext, Result,
    budget::Buffer,
    checking::{
        facts::{Atom, Callable, Fact, Facts, Node},
        scalar::Operation,
        slots::Slots,
    },
};

/// How a member renders the values nested in its receiver.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Rendering {
    /// `join`: nested arrays are joined in place and other values use native
    /// text conversion, which renders plain hash contents but refuses every
    /// executable value.
    Join,
    /// `inspect`: arrays and hashes are rendered structurally; only script
    /// functions are refused, while host methods and builtins print a marker.
    Inspect,
}

/// The conversion failures a rendering walk can reach.
struct Rendered {
    rejected: bool,
    throws: bool,
    unsupported: bool,
    possible: bool,
}

impl Rendering {
    fn refuses(self, facts: &Facts, value: Fact) -> bool {
        match self {
            Self::Join => matches!(
                facts.node(value),
                Node::Builtin(_) | Node::Callable { .. } | Node::Offset(_)
            ),
            Self::Inspect => matches!(
                facts.node(value),
                Node::Callable {
                    target: Callable::Function(_),
                    ..
                }
            ),
        }
    }
}

impl Facts {
    /// Walks every value nested in `receiver` that `rendering` converts. A
    /// position reached through literal tuples is converted on every path, so
    /// a value there that is certainly refused leaves no successful result.
    fn rendered(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        rendering: Rendering,
    ) -> Result<Rendered> {
        const UNCERTAIN: u8 = 1;
        const CERTAIN: u8 = 2;
        let mut result = Rendered {
            rejected: false,
            throws: false,
            unsupported: false,
            possible: true,
        };
        let mut visited = Slots::new(self.len(), 0u8);
        let mut pending = Buffer::empty();
        pending.push(ctx, (receiver, true))?;
        while let Some((value, certain)) = pending.data.pop() {
            ctx.charge(1)?;
            let mark = if certain { CERTAIN } else { UNCERTAIN };
            let seen = visited.get(ctx, value.0)?;
            if seen & mark != 0 {
                continue;
            }
            visited.set(ctx, value.0, seen | mark)?;
            match self.node(value) {
                Node::Tuple(items) => {
                    for &item in &items.data {
                        pending.push(ctx, (item, certain))?;
                    }
                }
                Node::Array(element) => pending.push(ctx, (*element, false))?,
                Node::Union(arms) => {
                    let mut refused = true;
                    for &arm in &arms.data {
                        refused &= rendering.refuses(self, arm) || arm == Atom::Never.fact();
                        pending.push(ctx, (arm, false))?;
                    }
                    result.possible &= !(certain && refused);
                }
                // Object hashes and protected data render as fixed text for `join`.
                Node::Hash(_, _, kind) | Node::Shape(_, _, _, kind)
                    if rendering == Rendering::Join && kind.object() => {}
                Node::Protected(..) if rendering == Rendering::Join => (),
                Node::Hash(_, element, _) | Node::Protected(element, ..) => {
                    pending.push(ctx, (*element, false))?;
                }
                Node::Shape(fields, open, ..) => {
                    result.throws |= *open;
                    for field in &fields.data {
                        pending.push(ctx, (field.value, false))?;
                    }
                }
                _ if rendering.refuses(self, value) => {
                    result.rejected = true;
                    result.possible &= !certain;
                }
                Node::Atom(Atom::Unknown | Atom::Any) => result.throws = true,
                Node::Named(_) | Node::Choice(_) => result.unsupported = true,
                _ => (),
            }
        }
        Ok(result)
    }

    /// Models `array.join(separator = "")`. The separator must be a string or
    /// symbol. Nested arrays are joined in place and every other element is
    /// converted natively without running a script `to_s`. The result is a
    /// general string.
    pub(super) fn join_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let mut possible = true;
        if let Some(&separator) = args.first() {
            let mut valid = false;
            for i in 0..self.arm_count(separator) {
                ctx.charge(1)?;
                let arm = self.arm(separator, i);
                match self.node(arm) {
                    Node::Atom(Atom::Never) => (),
                    Node::String(_) | Node::Symbol(_) | Node::Atom(Atom::String | Atom::Symbol) => {
                        valid = true;
                    }
                    Node::Atom(Atom::Unknown | Atom::Any) => {
                        valid = true;
                        result.throws = true;
                    }
                    Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                        result.unsupported = true;
                    }
                    _ => result.rejected = true,
                }
            }
            possible &= valid;
        }
        self.render_text(ctx, receiver, Rendering::Join, possible, result)
    }

    /// Models `array.inspect` and `hash.inspect` without arguments.
    pub(super) fn inspect_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
    ) -> Result<Operation> {
        let result = outcome(Atom::Never.fact());
        self.render_text(ctx, receiver, Rendering::Inspect, true, result)
    }

    fn render_text(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        rendering: Rendering,
        possible: bool,
        mut result: Operation,
    ) -> Result<Operation> {
        let rendered = self.rendered(ctx, receiver, rendering)?;
        result.rejected |= rendered.rejected;
        result.throws |= rendered.throws;
        result.unsupported |= rendered.unsupported;
        if possible && rendered.possible {
            result.value = Atom::String.fact();
        }
        Ok(result)
    }

    /// Models `array.flatten(depth = nil)`. A nil depth flattens every level,
    /// a negative depth does too, and zero copies the array. Depths follow
    /// the runtime's index conversion, so floats truncate and big integers
    /// fail. Literal tuples keep their order when every flattened level is
    /// itself a tuple.
    pub(super) fn flatten_member(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        args: &[Fact],
    ) -> Result<Operation> {
        let mut result = outcome(Atom::Never.fact());
        let mut depths = Buffer::empty();
        match args.first() {
            None => depths.push(ctx, Some(-1))?,
            Some(&depth) => {
                for i in 0..self.arm_count(depth) {
                    ctx.charge(1)?;
                    let arm = self.arm(depth, i);
                    if arm == Atom::Nil.fact() {
                        depths.push(ctx, Some(-1))?;
                        continue;
                    }
                    match self.count_arm(arm, false) {
                        Count::Exact(depth) => depths.push(ctx, Some(depth))?,
                        Count::Bounded(bounds) if bounds.max.is_some_and(|max| max < 0) => {
                            result.throws |= bounds.min.is_none();
                            depths.push(ctx, Some(-1))?;
                        }
                        Count::Bounded(bounds) => {
                            result.throws |= bounds.min.is_none() || bounds.max.is_none();
                            depths.push(ctx, None)?;
                        }
                        Count::Float | Count::Unknown => {
                            result.throws = true;
                            depths.push(ctx, None)?;
                        }
                        Count::Never => (),
                        Count::Invalid => result.rejected = true,
                        Count::Unsupported => result.unsupported = true,
                    }
                }
            }
        }
        for depth in depths.data {
            ctx.charge(1)?;
            let value = match depth {
                Some(depth) => {
                    // Beyond the fact depth every known level is flattened;
                    // unknown elements stay unknown either way.
                    let depth = if depth < 0 || depth as u64 > self.depth(receiver) as u64 {
                        -1
                    } else {
                        depth
                    };
                    match self.flattened_tuple(ctx, receiver, depth)? {
                        Some(value) => value,
                        None => self.flattened(ctx, receiver, depth, false, &mut result)?,
                    }
                }
                None => self.flattened(ctx, receiver, -1, true, &mut result)?,
            };
            result.value = self.union(ctx, &[result.value, value])?;
        }
        Ok(result)
    }

    /// Flattens a literal tuple in order, or returns `None` when a level that
    /// must be flattened is not certainly a tuple or certainly not an array.
    fn flattened_tuple(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        depth: i64,
    ) -> Result<Option<Fact>> {
        if !matches!(self.node(receiver), Node::Tuple(_)) {
            return Ok(None);
        }
        let mut output = Buffer::empty();
        let mut stack = Buffer::empty();
        stack.push(ctx, (receiver, 0usize, depth))?;
        while let Some(&(tuple, index, remaining)) = stack.data.last() {
            ctx.charge(1)?;
            let Node::Tuple(items) = self.node(tuple) else {
                unreachable!()
            };
            let Some(&item) = items.data.get(index) else {
                stack.data.pop();
                continue;
            };
            stack.data.last_mut().unwrap().1 += 1;
            if remaining == 0 {
                output.push(ctx, item)?;
                continue;
            }
            if matches!(self.node(item), Node::Tuple(_)) {
                let remaining = if remaining > 0 { remaining - 1 } else { -1 };
                stack.push(ctx, (item, 0, remaining))?;
                continue;
            }
            for i in 0..self.arm_count(item) {
                ctx.charge(1)?;
                let arm = self.arm(item, i);
                if matches!(
                    self.node(arm),
                    Node::Tuple(_)
                        | Node::Array(_)
                        | Node::Atom(Atom::Unknown | Atom::Any)
                        | Node::Named(_)
                        | Node::Choice(_)
                ) {
                    return Ok(None);
                }
            }
            output.push(ctx, item)?;
        }
        Ok(Some(self.tuple(ctx, &output.data)?))
    }

    /// The general array of values a flatten can produce, level by level.
    /// `containers` keeps each flattened array as a possible element, which
    /// describes an unknown depth.
    fn flattened(
        &mut self,
        ctx: &mut CallContext,
        receiver: Fact,
        depth: i64,
        containers: bool,
        result: &mut Operation,
    ) -> Result<Fact> {
        let mut leaves = Buffer::empty();
        let mut level = Buffer::empty();
        level.push(ctx, receiver)?;
        let mut remaining = depth;
        let mut visited = Slots::new(self.len(), false);
        let mut root = true;
        while !level.data.is_empty() {
            ctx.charge(1)?;
            let mut next = Buffer::empty();
            if depth >= 0 {
                // A finite depth distinguishes levels, so each level has its own memo.
                visited = Slots::new(self.len(), false);
            }
            for value in level.data.drain(..) {
                ctx.charge(1)?;
                for i in 0..self.arm_count(value) {
                    ctx.charge(1)?;
                    let arm = self.arm(value, i);
                    if visited.get(ctx, arm.0)? {
                        continue;
                    }
                    visited.set(ctx, arm.0, true)?;
                    let expand = root || remaining != 0;
                    match self.node(arm) {
                        Node::Atom(Atom::Never) => (),
                        Node::Tuple(items) if expand => {
                            for &item in &items.data {
                                next.push(ctx, item)?;
                            }
                            if containers && !root {
                                leaves.push(ctx, arm)?;
                            }
                        }
                        Node::Array(element) if expand => {
                            next.push(ctx, *element)?;
                            if containers && !root {
                                leaves.push(ctx, arm)?;
                            }
                        }
                        Node::Atom(Atom::Unknown | Atom::Any) => {
                            leaves.push(ctx, Atom::Unknown.fact())?;
                        }
                        Node::Named(_) | Node::Choice(_) if expand => result.unsupported = true,
                        _ => leaves.push(ctx, arm)?,
                    }
                }
            }
            if !root && remaining > 0 {
                remaining -= 1;
            }
            root = false;
            level = next;
        }
        let element = self.union(ctx, &leaves.data)?;
        if element == Atom::Never.fact() {
            self.tuple(ctx, &[])
        } else {
            self.array(ctx, element)
        }
    }
}
