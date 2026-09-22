use super::{
    facts::{Atom, Fact, Facts, Node},
    scalar::{Operation, Test},
};
use crate::{CallContext, Result, Value, budget::Buffer, value::Kind};

impl Facts {
    pub fn case_result(
        &mut self,
        ctx: &mut CallContext,
        target: Option<Fact>,
        matcher: Fact,
        splat: bool,
    ) -> Result<Operation> {
        ctx.checkpoint()?;
        let mut result = Operation {
            value: Atom::Never.fact(),
            rejected: false,
            unsupported: false,
            throws: false,
        };
        for t in 0..target.map_or(1, |target| self.arm_count(target)) {
            let target = target.map(|target| self.arm(target, t));
            for m in 0..self.arm_count(matcher) {
                ctx.charge(1)?;
                let matcher = self.arm(matcher, m);
                let value = if splat {
                    match self.node(matcher) {
                        Node::Atom(Atom::Never) => Atom::Never.fact(),
                        Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_) => Atom::Bool.fact(),
                        Node::Tuple(items) => {
                            let mut copied = Buffer::empty();
                            copied.extend(ctx, &items.data)?;
                            let mut maybe = false;
                            let mut certain = false;
                            for &item in &copied.data {
                                let matched = self.case_result(ctx, target, item, false)?.value;
                                match self.node(matched) {
                                    Node::Boolean(true) => {
                                        certain = true;
                                        break;
                                    }
                                    Node::Boolean(false) => (),
                                    _ => maybe = true,
                                }
                            }
                            if certain {
                                self.boolean(ctx, true)?
                            } else if maybe {
                                Atom::Bool.fact()
                            } else {
                                self.boolean(ctx, false)?
                            }
                        }
                        Node::Array(item) => {
                            let item = *item;
                            let matched = self.case_result(ctx, target, item, false)?.value;
                            if item == Atom::Never.fact()
                                || matches!(self.node(matched), Node::Boolean(false))
                            {
                                self.boolean(ctx, false)?
                            } else {
                                Atom::Bool.fact()
                            }
                        }
                        _ => {
                            result.rejected = true;
                            Atom::Never.fact()
                        }
                    }
                } else {
                    self.case_arm(ctx, target, matcher)?
                };
                result.value = self.union(ctx, &[result.value, value])?;
            }
        }
        Ok(result)
    }

    fn case_arm(
        &mut self,
        ctx: &mut CallContext,
        target: Option<Fact>,
        matcher: Fact,
    ) -> Result<Fact> {
        if matcher == Atom::Never.fact() || target == Some(Atom::Never.fact()) {
            return Ok(Atom::Never.fact());
        }
        let Some(target) = target else {
            return self.test_result(ctx, matcher, Test::Truth);
        };
        let numeric = matches!(self.atom(target), Some(Atom::Int | Atom::Float));
        let gradual = matches!(
            self.node(target),
            Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_)
        );
        let matched = match self.node(matcher) {
            Node::Range(start, end, exclusive) => {
                let (start, end, exclusive) = (*start, *end, *exclusive);
                match self.node(target) {
                    Node::Integer(value) => {
                        let value = *value;
                        let descending = matches!((start,end), (Some(a),Some(b)) if a>b);
                        Some(if descending {
                            start.is_none_or(|a| value <= a)
                                && end
                                    .is_none_or(|b| if exclusive { value > b } else { value >= b })
                        } else {
                            start.is_none_or(|a| value >= a)
                                && end
                                    .is_none_or(|b| if exclusive { value < b } else { value <= b })
                        })
                    }
                    Node::Float(bits) => Some(crate::range::contains_float(
                        start,
                        end,
                        exclusive,
                        f64::from_bits(*bits),
                    )),
                    // A range target compares by equality rather than membership.
                    Node::Range(a, b, x) => Some((*a, *b, *x) == (start, end, exclusive)),
                    _ if numeric && start.is_some() && start == end && exclusive => Some(false),
                    _ if numeric || gradual || self.atom(target) == Some(Atom::Range) => None,
                    _ => Some(false),
                }
            }
            Node::Atom(Atom::Range) => {
                if numeric || gradual || self.atom(target) == Some(Atom::Range) {
                    None
                } else {
                    Some(false)
                }
            }
            Node::Regex(value) => {
                if let Node::String(text) = self.node(target) {
                    let Kind::Regex(regex) = &value.0 else {
                        unreachable!()
                    };
                    Some(regex.matches(ctx, text)?)
                } else if self.atom(target) == Some(Atom::String) || gradual {
                    None
                } else {
                    Some(false)
                }
            }
            Node::Atom(Atom::Regex) => {
                if self.atom(target) == Some(Atom::String) || gradual {
                    None
                } else {
                    Some(false)
                }
            }
            _ => {
                let number = |value| match self.node(value) {
                    Node::Integer(n) => Some(Value::int(*n)),
                    Node::Float(n) => Some(Value::float(f64::from_bits(*n))),
                    _ => None,
                };
                if let (Some(a), Some(b)) = (number(target), number(matcher)) {
                    Some(crate::ops::equal(ctx, &a, &b, 0)?)
                } else if matches!(self.node(matcher), Node::Float(bits) if f64::from_bits(*bits).is_nan())
                {
                    Some(false)
                } else if let (Node::Atom(Atom::Float), Node::Integer(n)) =
                    (self.node(target), self.node(matcher))
                {
                    if crate::ops::equal(ctx, &Value::int(*n), &Value::float(*n as f64), 0)? {
                        None
                    } else {
                        Some(false)
                    }
                } else if matches!((self.node(target), self.node(matcher)), (Node::Atom(Atom::Int), Node::Float(bits)) if !f64::from_bits(*bits).is_finite() || f64::from_bits(*bits).fract()!=0.0)
                {
                    Some(false)
                } else {
                    self.definitely_equal(target, matcher)
                }
            }
        };
        match matched {
            Some(value) => self.boolean(ctx, value),
            None => Ok(Atom::Bool.fact()),
        }
    }

    pub fn case_filter(
        &mut self,
        ctx: &mut CallContext,
        value: Fact,
        matcher: Fact,
        splat: bool,
        yes: bool,
    ) -> Result<Fact> {
        ctx.checkpoint()?;
        let mut inputs = Buffer::empty();
        for i in 0..self.arm_count(value) {
            ctx.charge(1)?;
            let arm = self.arm(value, i);
            if arm == Atom::Bool.fact() {
                let no = self.boolean(ctx, false)?;
                let yes = self.boolean(ctx, true)?;
                inputs.extend(ctx, &[no, yes])?;
            } else if let Node::EnumMember {
                enumeration,
                index: None,
            } = self.node(arm)
            {
                let enumeration = *enumeration;
                let Node::Enumeration { value, .. } = self.node(enumeration) else {
                    unreachable!()
                };
                let Kind::Enum(value) = &value.0 else {
                    unreachable!()
                };
                let count = value.definition.members.len();
                for index in 0..count {
                    let member = self.enum_member(ctx, enumeration, index)?;
                    inputs.push(ctx, member)?;
                }
            } else {
                inputs.push(ctx, arm)?;
            }
        }
        let mut kept = Buffer::empty();
        for &arm in &inputs.data {
            let matched = self.case_result(ctx, Some(arm), matcher, splat)?.value;
            if matched == Atom::Never.fact()
                || matches!(self.node(matched), Node::Boolean(result) if *result!=yes)
            {
                continue;
            }
            let narrowed = if yes {
                self.case_positive(ctx, arm, matcher, splat)?
            } else {
                arm
            };
            kept.push(ctx, narrowed)?;
        }
        self.union(ctx, &kept.data)
    }

    fn case_positive(
        &mut self,
        ctx: &mut CallContext,
        source: Fact,
        matcher: Fact,
        splat: bool,
    ) -> Result<Fact> {
        let mut alternatives = Buffer::empty();
        for i in 0..self.arm_count(matcher) {
            ctx.charge(1)?;
            let matcher = self.arm(matcher, i);
            if splat {
                let items = match self.node(matcher) {
                    Node::Tuple(_) | Node::Array(_) => self.elements(ctx, matcher)?,
                    Node::Atom(Atom::Unknown | Atom::Any) | Node::Named(_) => Atom::Unknown.fact(),
                    _ => continue,
                };
                let value = self.case_positive(ctx, source, items, false)?;
                alternatives.push(ctx, value)?;
                continue;
            }
            let matched = self.case_result(ctx, Some(source), matcher, false)?.value;
            if matched == Atom::Never.fact() || matches!(self.node(matched), Node::Boolean(false)) {
                continue;
            }
            let value = if matches!(self.atom(source), Some(Atom::Unknown | Atom::Any)) {
                match self.node(matcher) {
                    Node::Integer(n) => {
                        let n = *n;
                        let float = n as f64;
                        if crate::ops::equal(ctx, &Value::int(n), &Value::float(float), 0)? {
                            let float = self.matching_float(ctx, float)?;
                            self.union(ctx, &[matcher, float])?
                        } else {
                            matcher
                        }
                    }
                    Node::IntegerBounds(_) | Node::Atom(Atom::Int | Atom::Float) => {
                        self.union(ctx, &[Atom::Int.fact(), Atom::Float.fact()])?
                    }
                    // A range matches its members and an equal range.
                    Node::Atom(Atom::Range) | Node::Range(..) => {
                        self.union(ctx, &[Atom::Int.fact(), Atom::Float.fact(), matcher])?
                    }
                    Node::Float(bits) => {
                        let value = f64::from_bits(*bits);
                        let matching = self.matching_float(ctx, value)?;
                        if value.is_finite() && value.fract() == 0.0 {
                            let integer = if value >= i64::MIN as f64 && value < -(i64::MIN as f64)
                            {
                                self.integer(ctx, value as i64)?
                            } else {
                                Atom::Int.fact()
                            };
                            self.union(ctx, &[integer, matching])?
                        } else {
                            matching
                        }
                    }
                    Node::Atom(Atom::Regex) | Node::Regex(_) => Atom::String.fact(),
                    Node::Array(_) | Node::Tuple(_) => self.array(ctx, Atom::Unknown.fact())?,
                    Node::Hash(..) | Node::Shape(..) => self.hash_kind(
                        ctx,
                        Atom::String.fact(),
                        Atom::Unknown.fact(),
                        self.plain_hash(matcher),
                    )?,
                    Node::Named(_) | Node::Nominal { .. } | Node::Instance { .. } => source,
                    _ => matcher,
                }
            } else if let (Node::Atom(Atom::Float), Node::Float(bits)) =
                (self.node(source), self.node(matcher))
            {
                self.matching_float(ctx, f64::from_bits(*bits))?
            } else if matches!(
                (self.node(source), self.node(matcher)),
                (Node::Atom(Atom::Int), Node::Integer(_))
                    | (Node::Atom(Atom::String), Node::String(_))
                    | (Node::Atom(Atom::Symbol), Node::Symbol(_))
            ) {
                matcher
            } else {
                source
            };
            alternatives.push(ctx, value)?;
        }
        self.union(ctx, &alternatives.data)
    }

    fn matching_float(&mut self, ctx: &mut CallContext, value: f64) -> Result<Fact> {
        let literal = self.float(ctx, value)?;
        if value == 0.0 {
            // Matching cannot distinguish signed zeros, but their literal facts must.
            let opposite = self.float(ctx, -value)?;
            self.union(ctx, &[literal, opposite])
        } else {
            Ok(literal)
        }
    }
}
