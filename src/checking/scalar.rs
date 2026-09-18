use super::facts::{Atom, Fact, Facts, Node};
use crate::{CallContext, Result, budget::Buffer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Test {
    Truth,
    Nil,
    Case { matcher: Fact, splat: bool },
}

pub(super) struct Operation {
    pub value: Fact,
    pub rejected: bool,
    pub unsupported: bool,
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
        if self.filter(ctx, value, test, true)? == Atom::Never.fact() {
            self.boolean(ctx, false)
        } else if self.filter(ctx, value, test, false)? == Atom::Never.fact() {
            self.boolean(ctx, true)
        } else {
            Ok(Atom::Bool.fact())
        }
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
        };
        for index in 0..self.arm_count(value) {
            ctx.charge(1)?;
            let arm = self.arm(value, index);
            let next = match (op, self.atom(arm)) {
                (_, Some(Atom::Never)) => Atom::Never.fact(),
                (_, Some(Atom::Unknown | Atom::Any)) => Atom::Unknown.fact(),
                ("+", Some(Atom::Int | Atom::Float | Atom::String)) => arm,
                ("-", Some(atom @ (Atom::Int | Atom::Float))) => {
                    if let Node::Integer(n) = self.node(arm) {
                        match n.checked_neg() {
                            Some(n) => self.integer(ctx, n)?,
                            None => atom.fact(),
                        }
                    } else if let Node::Float(bits) = self.node(arm) {
                        self.float(ctx, -f64::from_bits(*bits))?
                    } else {
                        atom.fact()
                    }
                }
                (_, None) => {
                    if matches!(
                        self.node(arm),
                        Node::Enumeration { .. } | Node::EnumMember { .. }
                    ) {
                        result.rejected = true;
                    } else {
                        result.unsupported = true;
                    }
                    Atom::Unknown.fact()
                }
                _ => {
                    result.rejected = true;
                    Atom::Unknown.fact()
                }
            };
            result.value = self.union(ctx, &[result.value, next])?;
        }
        Ok(result)
    }

    pub fn scalar_binary(
        &mut self,
        ctx: &mut CallContext,
        op: &str,
        left: Fact,
        right: Fact,
    ) -> Result<Operation> {
        let mut result = Operation {
            value: Atom::Never.fact(),
            rejected: false,
            unsupported: false,
        };
        if !matches!(
            op,
            "+" | "-"
                | "*"
                | "/"
                | "%"
                | "**"
                | "=="
                | "!="
                | "<"
                | "<="
                | ">"
                | ">="
                | "<=>"
                | "=~"
                | "!~"
                | "&"
        ) {
            result.unsupported = true;
            return Ok(result);
        }
        for a in 0..self.arm_count(left) {
            for b in 0..self.arm_count(right) {
                ctx.charge(1)?;
                let left = self.arm(left, a);
                let right = self.arm(right, b);
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
                        Node::Named(_) | Node::Nominal { .. } => {
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
                    result.value = self.union(ctx, &[result.value, next])?;
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
                    result.unsupported = true;
                    continue;
                };
                let next = if a == Atom::Never || b == Atom::Never {
                    Atom::Never.fact()
                } else if matches!(a, Atom::Any | Atom::Unknown)
                    || matches!(b, Atom::Any | Atom::Unknown)
                {
                    Atom::Unknown.fact()
                } else if matches!(op, "==" | "!=") {
                    if a == Atom::Nil || b == Atom::Nil {
                        self.boolean(ctx, (a == b) == (op == "=="))?
                    } else {
                        Atom::Bool.fact()
                    }
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
                result.value = self.union(ctx, &[result.value, next])?;
            }
        }
        Ok(result)
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
                    Node::Integer(_) => 1 << Atom::Int as u32,
                    Node::Float(_) => 1 << Atom::Int as u32,
                    Node::String(_) => 1 << Atom::String as u32,
                    Node::Symbol(_) => 1 << Atom::Symbol as u32,
                    Node::Range(..) => 1 << Atom::Range as u32,
                    Node::Regex(_) => 1 << Atom::Regex as u32,
                    Node::Array(_) | Node::Tuple(_) => 1 << 20,
                    Node::Hash(..) | Node::Shape(..) | Node::Protected(..) => 1 << 21,
                    Node::Builtin(_) | Node::Offset(_) => 1 << 22,
                    Node::TypeValue(_) => 1 << 23,
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
            Node::Integer(_) => Some(Atom::Int),
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
            Bool | Int | Float | String | Symbol | Duration | Time | Money | Regex
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
        "*" if a == String && b == Int => String,
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
