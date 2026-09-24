use super::facts::{Atom, Computation, Fact, Facts, Node};
use crate::{CallContext, Result, budget::Buffer};

/// The primitive binary operators that scalar analysis models.
const BINARY: [&str; 16] = [
    "+", "-", "*", "/", "%", "**", "==", "!=", "<", "<=", ">", ">=", "<=>", "=~", "!~", "&",
];

/// Identifies a modeled primitive binary operator for [`Computation`] keys.
pub(super) fn binary_code(op: &str) -> Option<u8> {
    BINARY
        .iter()
        .position(|&known| known == op)
        .map(|code| code as u8)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Test {
    Truth,
    Nil,
    Case {
        matcher: Fact,
        splat: bool,
    },
    Integer {
        comparison: super::integers::Comparison,
        other: Fact,
    },
}

pub(super) struct Operation {
    pub value: Fact,
    pub rejected: bool,
    pub unsupported: bool,
    /// A possible ordinary runtime failure that is not a known contradiction.
    pub throws: bool,
}

impl Operation {
    /// Packs the outcome flags for [`Facts::remember`].
    pub fn flags(&self) -> u16 {
        u16::from(self.rejected) | u16::from(self.unsupported) << 1 | u16::from(self.throws) << 2
    }

    /// Restores an outcome remembered with [`Self::flags`].
    pub fn remembered(value: Fact, flags: u16) -> Self {
        Self {
            value,
            rejected: flags & 1 != 0,
            unsupported: flags & 2 != 0,
            throws: flags & 4 != 0,
        }
    }
}

impl Facts {
    pub fn filter(
        &mut self,
        ctx: &mut CallContext,
        value: Fact,
        test: Test,
        yes: bool,
    ) -> Result<Fact> {
        if let Test::Case { matcher, splat } = test {
            return self.case_filter(ctx, value, matcher, splat, yes);
        }
        if let Test::Integer { comparison, other } = test {
            return self.filter_integers(ctx, value, comparison, other, yes);
        }
        let mut kept = Buffer::empty();
        for index in 0..self.arm_count(value) {
            ctx.charge(1)?;
            let arm = self.arm(value, index);
            let filtered = match (self.node(arm), test) {
                (Node::Atom(Atom::Never), _) => Atom::Never.fact(),
                (Node::Atom(Atom::Nil), _) => {
                    if yes == (test == Test::Nil) {
                        arm
                    } else {
                        Atom::Never.fact()
                    }
                }
                (Node::Boolean(value), Test::Truth) => {
                    if *value == yes {
                        arm
                    } else {
                        Atom::Never.fact()
                    }
                }
                (Node::Atom(Atom::Bool), Test::Truth) => self.boolean(ctx, yes)?,
                (Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_), _) => {
                    if test == Test::Nil && yes {
                        Atom::Nil.fact()
                    } else if test == Test::Truth && !yes {
                        let no = self.boolean(ctx, false)?;
                        self.union(ctx, &[no, Atom::Nil.fact()])?
                    } else {
                        arm
                    }
                }
                _ => {
                    if yes == (test == Test::Truth) {
                        arm
                    } else {
                        Atom::Never.fact()
                    }
                }
            };
            kept.push(ctx, filtered)?;
        }
        self.union(ctx, &kept.data)
    }

    pub fn test_result(&mut self, ctx: &mut CallContext, value: Fact, test: Test) -> Result<Fact> {
        // Every edge between blocks tests the truthiness of its top operand.
        let key = (test == Test::Truth).then_some(Computation::Truth(value));
        if let Some(key) = key {
            if let Some((result, _)) = self.remembered(ctx, key)? {
                return Ok(result);
            }
        }
        let result = if self.filter(ctx, value, test, true)? == Atom::Never.fact() {
            self.boolean(ctx, false)?
        } else if self.filter(ctx, value, test, false)? == Atom::Never.fact() {
            self.boolean(ctx, true)?
        } else {
            Atom::Bool.fact()
        };
        if let Some(key) = key {
            self.remember(ctx, key, (result, 0))?;
        }
        Ok(result)
    }

    pub fn scalar_unary(
        &mut self,
        ctx: &mut CallContext,
        op: &str,
        value: Fact,
    ) -> Result<Operation> {
        let mut result = Operation {
            value: Atom::Never.fact(),
            rejected: false,
            unsupported: false,
            throws: false,
        };
        let mut values = Buffer::empty();
        for index in 0..self.arm_count(value) {
            ctx.charge(1)?;
            let arm = self.arm(value, index);
            let next = match (op, self.atom(arm)) {
                (_, Some(Atom::Never)) => Atom::Never.fact(),
                (_, Some(Atom::Unknown | Atom::Any)) => Atom::Unknown.fact(),
                ("+", Some(Atom::Int | Atom::Float | Atom::String)) => arm,
                ("-", Some(atom @ (Atom::Int | Atom::Float))) => {
                    if let Some(bounds) = self.integer_bounds(arm) {
                        self.integer_range(ctx, bounds.negate())?
                    } else if let Node::Float(bits) = self.node(arm) {
                        self.float(ctx, -f64::from_bits(*bits))?
                    } else {
                        atom.fact()
                    }
                }
                (_, None) => {
                    if matches!(
                        self.node(arm),
                        Node::Named(_) | Node::Nominal { .. } | Node::Choice(_)
                    ) {
                        result.unsupported = true;
                    } else {
                        // Unary operators never dispatch to source methods, and no
                        // container, object, enum or callable has a native sign.
                        result.rejected = true;
                    }
                    Atom::Unknown.fact()
                }
                _ => {
                    result.rejected = true;
                    Atom::Unknown.fact()
                }
            };
            values.push(ctx, next)?;
        }
        // Keep any result joined into the outcome directly.
        values.push(ctx, result.value)?;
        result.value = self.union(ctx, &values.data)?;
        Ok(result)
    }

    pub fn scalar_binary(
        &mut self,
        ctx: &mut CallContext,
        op: &str,
        left: Fact,
        right: Fact,
    ) -> Result<(Operation, bool)> {
        let Some(code) = binary_code(op) else {
            return self.binary_uncached(ctx, op, left, right);
        };
        // Loops and repeated block walks apply the same operator to the same facts again.
        let key = Computation::Binary(code, left, right);
        if let Some((value, flags)) = self.remembered(ctx, key)? {
            return Ok((Operation::remembered(value, flags), flags & 8 != 0));
        }
        let (result, limit) = self.binary_uncached(ctx, op, left, right)?;
        let flags = result.flags() | u16::from(limit) << 3;
        self.remember(ctx, key, (result.value, flags))?;
        Ok((result, limit))
    }

    fn binary_uncached(
        &mut self,
        ctx: &mut CallContext,
        op: &str,
        left: Fact,
        right: Fact,
    ) -> Result<(Operation, bool)> {
        let mut result = Operation {
            value: Atom::Never.fact(),
            rejected: false,
            unsupported: false,
            throws: false,
        };
        let mut limit = false;
        // Collect the pairwise results for one union instead of interning every partial join.
        let mut values = Buffer::empty();
        if !BINARY.contains(&op) {
            result.unsupported = true;
            return Ok((result, limit));
        }
        let comparison = super::integers::Comparison::parse(op);
        // Integer comparisons can only add false and true; once both appear, later integer
        // pairs cannot change the result.
        let mut compared = 0u8;
        for a in 0..self.arm_count(left) {
            for b in 0..self.arm_count(right) {
                ctx.charge(1)?;
                let left = self.arm(left, a);
                let right = self.arm(right, b);
                if compared == 3
                    && comparison.is_some()
                    && self.integer_bounds(left).is_some()
                    && self.integer_bounds(right).is_some()
                {
                    continue;
                }
                let array = |value| matches!(self.node(value), Node::Array(_) | Node::Tuple(_));
                let arrays = [array(left), array(right)];
                if matches!(op, "-" | "&") && arrays[0] {
                    let next = self.array_set(ctx, op, left, right)?;
                    values.push(ctx, next.value)?;
                    result.rejected |= next.rejected;
                    result.unsupported |= next.unsupported;
                    continue;
                }
                if op == "+" && (arrays[0] || arrays[1]) {
                    if !arrays[0] || !arrays[1] {
                        let other = if arrays[0] { right } else { left };
                        let next = match self.node(other) {
                            Node::Atom(Atom::Never) => Atom::Never.fact(),
                            Node::Atom(Atom::Any | Atom::Unknown) => Atom::Unknown.fact(),
                            Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                                result.unsupported = true;
                                continue;
                            }
                            _ => {
                                result.rejected = true;
                                Atom::Never.fact()
                            }
                        };
                        values.push(ctx, next)?;
                        continue;
                    }
                    let next = if let (Node::Tuple(a), Node::Tuple(b)) =
                        (self.node(left), self.node(right))
                    {
                        let mut elements = Buffer::empty();
                        elements.extend(ctx, &a.data)?;
                        elements.extend(ctx, &b.data)?;
                        self.tuple(ctx, &elements.data)?
                    } else {
                        let left = self.elements(ctx, left)?;
                        let right = self.elements(ctx, right)?;
                        let element = self.union(ctx, &[left, right])?;
                        self.array(ctx, element)?
                    };
                    values.push(ctx, next)?;
                    continue;
                }
                if op == "%" && self.atom(left) == Some(Atom::String) && right != Atom::Never.fact()
                {
                    values.push(ctx, Atom::String.fact())?;
                    continue;
                }
                if matches!(op, "==" | "!=")
                    && (matches!(self.node(left), Node::Instance { .. } | Node::TypeValue(_))
                        || matches!(self.node(right), Node::Instance { .. } | Node::TypeValue(_)))
                {
                    if matches!(
                        self.node(left),
                        Node::Named(_) | Node::Nominal { .. } | Node::Choice(_)
                    ) {
                        result.unsupported = true;
                        continue;
                    }
                    // Only the receiver can dispatch a source operator with an arbitrary result.
                    let next = if matches!(self.node(left), Node::Atom(Atom::Unknown | Atom::Any)) {
                        Atom::Unknown.fact()
                    } else if matches!(
                        self.node(right),
                        Node::Atom(Atom::Unknown | Atom::Any)
                            | Node::Named(_)
                            | Node::Nominal { .. }
                            | Node::Choice(_)
                    ) {
                        Atom::Bool.fact()
                    } else if left == Atom::Never.fact() || right == Atom::Never.fact() {
                        Atom::Never.fact()
                    } else if matches!(self.node(left), Node::Instance { .. })
                        && matches!(self.node(right), Node::Instance { .. })
                    {
                        match self.definitely_equal(left, right) {
                            Some(equal) => self.boolean(ctx, equal == (op == "=="))?,
                            None => Atom::Bool.fact(),
                        }
                    } else {
                        self.boolean(ctx, (left == right) == (op == "=="))?
                    };
                    values.push(ctx, next)?;
                    continue;
                }
                let structural = |value| {
                    matches!(
                        self.node(value),
                        Node::Array(_)
                            | Node::Tuple(_)
                            | Node::Hash(..)
                            | Node::Shape(..)
                            | Node::Protected(..)
                    )
                };
                if matches!(op, "==" | "!=") && (structural(left) || structural(right)) {
                    match self.node(left) {
                        // An unknown receiver may dispatch a source operator with any result type.
                        Node::Atom(Atom::Unknown | Atom::Any) => {
                            values.push(ctx, Atom::Unknown.fact())?;
                            continue;
                        }
                        Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                            result.unsupported = true;
                            continue;
                        }
                        _ => (),
                    }
                    let (mut next, guarded) = self.value_equal(ctx, left, right)?;
                    limit |= guarded;
                    if op == "!=" {
                        if let Node::Boolean(value) = self.node(next) {
                            next = self.boolean(ctx, !value)?;
                        }
                    }
                    values.push(ctx, next)?;
                    continue;
                }
                let enumeration = |value| {
                    matches!(
                        self.node(value),
                        Node::Enumeration { .. } | Node::EnumMember { .. }
                    )
                };
                if enumeration(left) || enumeration(right) {
                    let other = if enumeration(left) { right } else { left };
                    let next = match self.node(other) {
                        Node::Atom(Atom::Never) => Atom::Never.fact(),
                        Node::Named(_) | Node::Nominal { .. } | Node::Choice(_) => {
                            result.unsupported = true;
                            continue;
                        }
                        Node::Atom(Atom::Unknown | Atom::Any) => Atom::Unknown.fact(),
                        _ if matches!(op, "==" | "!=") => {
                            match self.definitely_equal(left, right) {
                                Some(equal) => self.boolean(ctx, equal == (op == "=="))?,
                                None => Atom::Bool.fact(),
                            }
                        }
                        _ if op == "<=>" => Atom::Nil.fact(),
                        _ if op == "%" && self.atom(left) == Some(Atom::String) => {
                            Atom::String.fact()
                        }
                        Node::String(_) | Node::Atom(Atom::String)
                            if op == "+"
                                && (self.enum_nominal(left).is_some()
                                    || self.enum_nominal(right).is_some()) =>
                        {
                            Atom::String.fact()
                        }
                        _ => {
                            result.rejected = true;
                            Atom::Never.fact()
                        }
                    };
                    values.push(ctx, next)?;
                    continue;
                }
                if op == "+"
                    && ((matches!(self.node(left), Node::Protected(..))
                        && self.atom(right) == Some(Atom::String))
                        || (self.atom(left) == Some(Atom::String)
                            && matches!(self.node(right), Node::Protected(..))))
                {
                    result.rejected = true;
                    continue;
                }
                let (Some(a), Some(b)) = (self.atom(left), self.atom(right)) else {
                    let next =
                        self.opaque_binary(ctx, op, left, right, &mut result.rejected, &mut limit)?;
                    match next {
                        Some(next) => values.push(ctx, next)?,
                        None => result.unsupported = true,
                    }
                    continue;
                };
                if let (Some(a), Some(b)) = (self.integer_bounds(left), self.integer_bounds(right))
                {
                    if let Some(bounds) = a.arithmetic(op, b) {
                        let next = self.integer_range(ctx, bounds)?;
                        values.push(ctx, next)?;
                        continue;
                    }
                    if let Some(comparison) = comparison {
                        let next = match a.compare(comparison, b) {
                            Some(value) => {
                                compared |= 1 << u8::from(value);
                                self.boolean(ctx, value)?
                            }
                            None => {
                                compared = 3;
                                Atom::Bool.fact()
                            }
                        };
                        values.push(ctx, next)?;
                        continue;
                    }
                }
                let next = if a == Atom::Never || b == Atom::Never {
                    Atom::Never.fact()
                } else if matches!(a, Atom::Any | Atom::Unknown) {
                    Atom::Unknown.fact()
                } else if matches!(op, "==" | "!=") {
                    let (mut value, guarded) = self.value_equal(ctx, left, right)?;
                    limit |= guarded;
                    if op == "!=" {
                        if let Node::Boolean(equal) = self.node(value) {
                            value = self.boolean(ctx, !equal)?;
                        }
                    }
                    value
                } else if matches!(b, Atom::Any | Atom::Unknown) {
                    Atom::Unknown.fact()
                } else if op == "<=>" {
                    if a == Atom::Nil && b == Atom::Nil {
                        self.integer(ctx, 0)?
                    } else if primitive_binary("<", a, b).is_some() {
                        if matches!(a, Atom::Money | Atom::Float) || b == Atom::Float {
                            self.nullable(ctx, Atom::Int.fact())?
                        } else {
                            Atom::Int.fact()
                        }
                    } else {
                        Atom::Nil.fact()
                    }
                } else if let Some(atom) = primitive_binary(op, a, b) {
                    if op == "=~" {
                        self.nullable(ctx, Atom::Int.fact())?
                    } else if op == "**" && a == Atom::Int && b == Atom::Int {
                        match self.node(right) {
                            Node::Integer(exponent) if *exponent >= 0 => Atom::Int.fact(),
                            Node::Integer(_) => Atom::Float.fact(),
                            _ => self.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?,
                        }
                    } else {
                        atom.fact()
                    }
                } else {
                    result.rejected = true;
                    Atom::Unknown.fact()
                };
                values.push(ctx, next)?;
            }
        }
        // Keep any result joined into the outcome directly.
        values.push(ctx, result.value)?;
        result.value = self.union(ctx, &values.data)?;
        Ok((result, limit))
    }

    /// Applies a native binary operator when an operand is a container, object,
    /// type or callable rather than a scalar.
    ///
    /// None of these values has native arithmetic or order, so beyond the array,
    /// enum and format cases handled earlier only equality and `<=>` can succeed.
    /// Returns `None` when a type contract has not been resolved to its values.
    fn opaque_binary(
        &mut self,
        ctx: &mut CallContext,
        op: &str,
        left: Fact,
        right: Fact,
        rejected: &mut bool,
        limit: &mut bool,
    ) -> Result<Option<Fact>> {
        let contract = |value| {
            matches!(
                self.node(value),
                Node::Named(_) | Node::Nominal { .. } | Node::Choice(_)
            )
        };
        if contract(left) || contract(right) {
            return Ok(None);
        }
        if left == Atom::Never.fact() || right == Atom::Never.fact() {
            return Ok(Some(Atom::Never.fact()));
        }
        let gradual = |value| matches!(self.node(value), Node::Atom(Atom::Unknown | Atom::Any));
        // An unknown receiver may dispatch a source operator with any result type.
        if gradual(left) {
            return Ok(Some(Atom::Unknown.fact()));
        }
        if matches!(op, "==" | "!=") {
            let (mut value, guarded) = self.value_equal(ctx, left, right)?;
            *limit |= guarded;
            if op == "!=" {
                if let Node::Boolean(equal) = self.node(value) {
                    value = self.boolean(ctx, !equal)?;
                }
            }
            return Ok(Some(value));
        }
        if gradual(right) {
            return Ok(Some(Atom::Unknown.fact()));
        }
        if op == "<=>" {
            return self.opaque_order(ctx, left, right, limit).map(Some);
        }
        *rejected = true;
        Ok(Some(Atom::Unknown.fact()))
    }

    /// Models `<=>` beside a non-scalar operand: two arrays compare element by
    /// element, and every other pair has no order.
    fn opaque_order(
        &mut self,
        ctx: &mut CallContext,
        left: Fact,
        right: Fact,
        limit: &mut bool,
    ) -> Result<Fact> {
        use super::ordering::{EQUAL, GREATER, LESS, UNORDERED};
        let array = |value| matches!(self.node(value), Node::Array(_) | Node::Tuple(_));
        if !array(left) || !array(right) {
            return Ok(Atom::Nil.fact());
        }
        *limit |= self.may_exceed_depth(ctx, left, 0)? && self.may_exceed_depth(ctx, right, 0)?;
        let order = self.order_result(ctx, left, right)?;
        let mut values = Buffer::empty();
        for (bit, sign) in [(LESS, -1), (EQUAL, 0), (GREATER, 1)] {
            if order & bit != 0 {
                let value = self.integer(ctx, sign)?;
                values.push(ctx, value)?;
            }
        }
        if order & UNORDERED != 0 {
            values.push(ctx, Atom::Nil.fact())?;
        }
        self.union(ctx, &values.data)
    }

    pub fn known_non_callable(&self, ctx: &mut CallContext, value: Fact) -> Result<bool> {
        for index in 0..self.arm_count(value) {
            ctx.charge(1)?;
            let arm = self.arm(value, index);
            if matches!(
                self.node(arm),
                Node::Array(_)
                    | Node::Tuple(_)
                    | Node::Hash(..)
                    | Node::Shape(..)
                    | Node::Protected(..)
                    | Node::TypeValue(_)
                    | Node::Instance { .. }
                    | Node::Enumeration { .. }
                    | Node::EnumMember { .. }
            ) {
                continue;
            }
            if matches!(self.atom(arm), None | Some(Atom::Unknown | Atom::Any)) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn known_primitive(&self, ctx: &mut CallContext, value: Fact) -> Result<bool> {
        for index in 0..self.arm_count(value) {
            ctx.charge(1)?;
            if matches!(
                self.atom(self.arm(value, index)),
                None | Some(Atom::Unknown | Atom::Any)
            ) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn known_nil_receiver(&self, ctx: &mut CallContext, value: Fact) -> Result<bool> {
        for i in 0..self.arm_count(value) {
            ctx.charge(1)?;
            let arm = self.arm(value, i);
            if !matches!(
                self.node(arm),
                Node::Protected(..)
                    | Node::Tuple(_)
                    | Node::Array(_)
                    | Node::Enumeration { .. }
                    | Node::EnumMember { .. }
            ) && !self.plain_hash(arm)
                && !self.known_primitive(ctx, arm)?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub fn reassignment_conflicts(
        &self,
        ctx: &mut CallContext,
        before: Fact,
        after: Fact,
    ) -> Result<bool> {
        let kinds = |value| -> Result<Option<u32>> {
            let mut bits = 0;
            for index in 0..self.arm_count(value) {
                ctx.charge(1)?;
                let bit = match self.node(self.arm(value, index)) {
                    Node::Atom(Atom::Never | Atom::Nil) => 0,
                    Node::Atom(Atom::Unknown | Atom::Any)
                    | Node::Named(_)
                    | Node::Nominal { .. }
                    | Node::Choice(_) => return Ok(None),
                    Node::Atom(Atom::Int | Atom::Float) => 1 << Atom::Int as u32,
                    Node::Atom(atom) => 1 << *atom as u32,
                    Node::Boolean(_) => 1 << Atom::Bool as u32,
                    Node::Integer(_) | Node::IntegerBounds(_) => 1 << Atom::Int as u32,
                    Node::Float(_) => 1 << Atom::Int as u32,
                    Node::String(_) => 1 << Atom::String as u32,
                    Node::Symbol(_) => 1 << Atom::Symbol as u32,
                    Node::Range(..) => 1 << Atom::Range as u32,
                    Node::Regex(_) => 1 << Atom::Regex as u32,
                    Node::Array(_) | Node::Tuple(_) => 1 << 20,
                    Node::Hash(..) | Node::Shape(..) | Node::Protected(..) => 1 << 21,
                    Node::Builtin(_) | Node::Offset(_) => 1 << 22,
                    Node::Callable {
                        target: super::facts::Callable::Host(_),
                        ..
                    } => 1 << 22,
                    Node::Callable {
                        target: super::facts::Callable::Function(_),
                        ..
                    } => 1 << 26,
                    Node::TypeValue(_) => 1 << 23,
                    Node::Instance { .. } => 1 << 27,
                    Node::Enumeration { .. } => 1 << 24,
                    Node::EnumMember { .. } => 1 << 25,
                    Node::Union(_) => unreachable!(),
                };
                bits |= bit;
            }
            Ok(Some(bits))
        };
        let mut kinds = kinds;
        let (Some(before), Some(after)) = (kinds(before)?, kinds(after)?) else {
            return Ok(false);
        };
        Ok(before != 0 && after != 0 && before & after == 0)
    }

    pub(super) fn arm_count(&self, value: Fact) -> usize {
        if let Node::Union(arms) = self.node(value) {
            arms.data.len()
        } else {
            1
        }
    }

    pub(super) fn arm(&self, value: Fact, index: usize) -> Fact {
        if let Node::Union(arms) = self.node(value) {
            arms.data[index]
        } else {
            value
        }
    }

    pub(super) fn atom(&self, value: Fact) -> Option<Atom> {
        match self.node(value) {
            Node::Atom(atom) => Some(*atom),
            Node::Boolean(_) => Some(Atom::Bool),
            Node::Integer(_) | Node::IntegerBounds(_) => Some(Atom::Int),
            Node::Float(_) => Some(Atom::Float),
            Node::String(_) => Some(Atom::String),
            Node::Symbol(_) => Some(Atom::Symbol),
            Node::Range(..) => Some(Atom::Range),
            Node::Regex(_) => Some(Atom::Regex),
            _ => None,
        }
    }
}

fn primitive_binary(op: &str, a: Atom, b: Atom) -> Option<Atom> {
    use Atom::*;
    let number = |atom| matches!(atom, Int | Float);
    let printable = |atom| {
        matches!(
            atom,
            Bool | Int | Float | String | Symbol | Duration | Time | Money | Regex | Range
        )
    };
    let numeric = if a == Int && b == Int { Int } else { Float };
    Some(match op {
        "=~" | "!~" if (a == Regex && b == String) || (a == String && b == Regex) => {
            if op == "=~" {
                Int
            } else {
                Bool
            }
        }
        "+" | "-" | "*" | "/" | "**" if number(a) && number(b) => numeric,
        "%" if a == Int && b == Int => Int,
        "+" if (a == String || b == String) && printable(a) && printable(b) => String,
        "*" if a == String && number(b) => String,
        "%" if a == String => String,
        "+" if (a == Time && (b == Duration || number(b)))
            || (b == Time && (a == Duration || number(a))) =>
        {
            Time
        }
        "-" if a == Time && b == Time => Float,
        "-" if a == Time && (b == Duration || number(b)) => Time,
        "+" | "-" if a == Duration && (b == Duration || number(b)) => Duration,
        "+" if number(a) && b == Duration => Duration,
        "*" if (a == Duration && number(b)) || (number(a) && b == Duration) => Duration,
        "/" if a == Duration && b == Duration => Float,
        "/" if a == Duration && number(b) => Duration,
        "%" if a == Duration && b == Duration => Duration,
        "+" | "-" if a == Money && b == Money => Money,
        "*" if (a == Money && b == Int) || (a == Int && b == Money) => Money,
        "/" if a == Money && b == Int => Money,
        "<" | "<=" | ">" | ">="
            if (number(a) && number(b))
                || (a == b && matches!(a, String | Symbol | Money | Duration | Time)) =>
        {
            Bool
        }
        _ => return None,
    })
}
