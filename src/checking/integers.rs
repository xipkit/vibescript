use super::facts::{Atom, Fact, Facts, Node};
use crate::{CallContext, Result, budget::Buffer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(super) struct Bounds {
    pub min: Option<i64>,
    pub max: Option<i64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Comparison {
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
    Equal,
    NotEqual,
}

impl Comparison {
    pub fn parse(op: &str) -> Option<Self> {
        Some(match op {
            "<" => Self::Less,
            "<=" => Self::LessEqual,
            ">" => Self::Greater,
            ">=" => Self::GreaterEqual,
            "==" => Self::Equal,
            "!=" => Self::NotEqual,
            _ => return None,
        })
    }

    pub fn reversed(self) -> Self {
        match self {
            Self::Less => Self::Greater,
            Self::LessEqual => Self::GreaterEqual,
            Self::Greater => Self::Less,
            Self::GreaterEqual => Self::LessEqual,
            value => value,
        }
    }

    fn negated(self) -> Self {
        match self {
            Self::Less => Self::GreaterEqual,
            Self::LessEqual => Self::Greater,
            Self::Greater => Self::LessEqual,
            Self::GreaterEqual => Self::Less,
            Self::Equal => Self::NotEqual,
            Self::NotEqual => Self::Equal,
        }
    }
}

impl Bounds {
    pub const ALL: Self = Self {
        min: None,
        max: None,
    };

    pub fn point(value: i64) -> Self {
        Self {
            min: Some(value),
            max: Some(value),
        }
    }

    pub fn contains(self, other: Self) -> bool {
        self.min.is_none_or(|n| other.min.is_some_and(|m| n <= m))
            && self.max.is_none_or(|n| other.max.is_some_and(|m| n >= m))
    }

    pub fn includes(self, value: i128) -> bool {
        self.min.is_none_or(|n| i128::from(n) <= value)
            && self.max.is_none_or(|n| i128::from(n) >= value)
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let min = match (self.min, other.min) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        let max = match (self.max, other.max) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if matches!((min, max), (Some(a), Some(b)) if a > b) {
            None
        } else {
            Some(Self { min, max })
        }
    }

    pub fn hull(self, other: Self) -> Self {
        Self {
            min: self.min.zip(other.min).map(|(a, b)| a.min(b)),
            max: self.max.zip(other.max).map(|(a, b)| a.max(b)),
        }
    }

    pub fn widen(self, other: Self) -> Self {
        Self {
            min: self.min.filter(|&a| other.min.is_some_and(|b| b >= a)),
            max: self.max.filter(|&a| other.max.is_some_and(|b| b <= a)),
        }
    }

    // Bounds outside compact integers are rounded outward: script arithmetic
    // promotes to arbitrary precision rather than wrapping at i64 endpoints.
    fn lower(value: i128) -> Option<i64> {
        if value < i128::from(i64::MIN) {
            None
        } else {
            Some(value.min(i128::from(i64::MAX)) as i64)
        }
    }

    fn upper(value: i128) -> Option<i64> {
        if value > i128::from(i64::MAX) {
            None
        } else {
            Some(value.max(i128::from(i64::MIN)) as i64)
        }
    }

    pub fn negate(self) -> Self {
        Self {
            min: self.max.and_then(|n| Self::lower(-i128::from(n))),
            max: self.min.and_then(|n| Self::upper(-i128::from(n))),
        }
    }

    pub fn arithmetic(self, op: &str, other: Self) -> Option<Self> {
        Some(match op {
            "+" => Self {
                min: self
                    .min
                    .zip(other.min)
                    .and_then(|(a, b)| Self::lower(i128::from(a) + i128::from(b))),
                max: self
                    .max
                    .zip(other.max)
                    .and_then(|(a, b)| Self::upper(i128::from(a) + i128::from(b))),
            },
            "-" => return self.arithmetic("+", other.negate()),
            "*" => {
                if self == Self::point(0) || other == Self::point(0) {
                    return Some(Self::point(0));
                }
                let (Some(a), Some(b), Some(c), Some(d)) =
                    (self.min, self.max, other.min, other.max)
                else {
                    return Some(Self::ALL);
                };
                let products = [
                    i128::from(a) * i128::from(c),
                    i128::from(a) * i128::from(d),
                    i128::from(b) * i128::from(c),
                    i128::from(b) * i128::from(d),
                ];
                Self {
                    min: Self::lower(*products.iter().min().unwrap()),
                    max: Self::upper(*products.iter().max().unwrap()),
                }
            }
            _ => return None,
        })
    }

    pub fn compare(self, comparison: Comparison, other: Self) -> Option<bool> {
        use Comparison::*;
        match comparison {
            Greater | GreaterEqual => other.compare(comparison.reversed(), self),
            Less => {
                if self.max.zip(other.min).is_some_and(|(a, b)| a < b) {
                    Some(true)
                } else if self.min.zip(other.max).is_some_and(|(a, b)| a >= b) {
                    Some(false)
                } else {
                    None
                }
            }
            LessEqual => {
                if self.max.zip(other.min).is_some_and(|(a, b)| a <= b) {
                    Some(true)
                } else if self.min.zip(other.max).is_some_and(|(a, b)| a > b) {
                    Some(false)
                } else {
                    None
                }
            }
            Equal => {
                if self.intersection(other).is_none() {
                    Some(false)
                } else if self.min.is_some() && self.min == self.max && self == other {
                    Some(true)
                } else {
                    None
                }
            }
            NotEqual => self.compare(Equal, other).map(|value| !value),
        }
    }

    fn filter(self, comparison: Comparison, other: Self) -> Option<Self> {
        use Comparison::*;
        let constraint = match comparison {
            Less => Self {
                min: None,
                max: other.max.and_then(|n| Self::upper(i128::from(n) - 1)),
            },
            LessEqual => Self {
                min: None,
                max: other.max,
            },
            Greater => Self {
                min: other.min.and_then(|n| Self::lower(i128::from(n) + 1)),
                max: None,
            },
            GreaterEqual => Self {
                min: other.min,
                max: None,
            },
            Equal => other,
            NotEqual => {
                if other.min.is_none() || other.min != other.max {
                    return Some(self);
                }
                let n = other.min.unwrap();
                if self == other {
                    return None;
                }
                if self.min == Some(n) {
                    Self {
                        min: Self::lower(i128::from(n) + 1),
                        max: None,
                    }
                } else if self.max == Some(n) {
                    Self {
                        min: None,
                        max: Self::upper(i128::from(n) - 1),
                    }
                } else {
                    return Some(self);
                }
            }
        };
        self.intersection(constraint)
    }
}

impl Facts {
    pub(super) fn integer_thresholds(
        &self,
        ctx: &mut CallContext,
        program: &crate::bytecode::Program,
    ) -> Result<Buffer<i64>> {
        let mut thresholds = Buffer::empty();
        thresholds.extend(ctx, &[-1, 0, 1])?;
        // Source constants are fixed across repeated call-summary evaluations.
        // Facts produced while solving a retry must not expand this finite set.
        for value in &program.constants {
            ctx.charge(1)?;
            if let Some(value) = value.as_int() {
                thresholds.push(ctx, value)?;
                if let Some(negated) = value.checked_neg() {
                    thresholds.push(ctx, negated)?;
                }
            }
        }
        for function in &program.functions {
            for op in &function.code {
                ctx.charge(1)?;
                if let crate::bytecode::Op::Array(count) = op {
                    if let Ok(count) = i64::try_from(*count) {
                        thresholds.push(ctx, count)?;
                        thresholds.push(ctx, -count)?;
                    }
                }
            }
        }
        ctx.charge(
            thresholds
                .data
                .len()
                .saturating_mul(thresholds.data.len().max(1).ilog2() as usize + 1)
                as u64,
        )?;
        thresholds.data.sort_unstable();
        thresholds.data.dedup();
        Ok(thresholds)
    }

    pub(super) fn widen_integer_thresholds(
        &mut self,
        ctx: &mut CallContext,
        before: Fact,
        after: Fact,
        thresholds: &[i64],
    ) -> Result<Option<Fact>> {
        if before == after {
            ctx.charge(1)?;
            return Ok(Some(before));
        }
        let (Some(a), Some(b)) = (
            self.integer_hull(ctx, before)?,
            self.integer_hull(ctx, after)?,
        ) else {
            return Ok(None);
        };
        ctx.charge(2 * u64::from(thresholds.len().max(1).ilog2() + 1))?;
        let mut widened = a.widen(b);
        if widened.min.is_none() && a.min.is_some() {
            if let Some(bound) = b.min {
                let end = thresholds.partition_point(|&n| n <= bound);
                widened.min = end.checked_sub(1).map(|i| thresholds[i]);
            }
        }
        if widened.max.is_none() && a.max.is_some() {
            if let Some(bound) = b.max {
                widened.max = thresholds
                    .get(thresholds.partition_point(|&n| n < bound))
                    .copied();
            }
        }
        self.integer_range(ctx, widened).map(Some)
    }

    pub(super) fn integer_bounds(&self, value: Fact) -> Option<Bounds> {
        match self.node(value) {
            Node::Atom(Atom::Int) => Some(Bounds::ALL),
            Node::Integer(value) => Some(Bounds::point(*value)),
            Node::IntegerBounds(bounds) => Some(*bounds),
            _ => None,
        }
    }

    pub(super) fn integer_hull(
        &self,
        ctx: &mut CallContext,
        value: Fact,
    ) -> Result<Option<Bounds>> {
        let mut result = None;
        for i in 0..self.arm_count(value) {
            ctx.charge(1)?;
            let Some(bounds) = self.integer_bounds(self.arm(value, i)) else {
                return Ok(None);
            };
            result = Some(result.map_or(bounds, |previous: Bounds| previous.hull(bounds)));
        }
        Ok(result)
    }

    pub(super) fn filter_integers(
        &mut self,
        ctx: &mut CallContext,
        value: Fact,
        comparison: Comparison,
        other: Fact,
        yes: bool,
    ) -> Result<Fact> {
        let Some(other) = self.integer_hull(ctx, other)? else {
            return Ok(value);
        };
        let comparison = if yes {
            comparison
        } else {
            comparison.negated()
        };
        let mut values = Buffer::empty();
        for i in 0..self.arm_count(value) {
            ctx.charge(1)?;
            let value = self.arm(value, i);
            let value = if let Some(bounds) = self.integer_bounds(value) {
                match bounds.filter(comparison, other) {
                    Some(bounds) => self.integer_range(ctx, bounds)?,
                    None => Atom::Never.fact(),
                }
            } else {
                value
            };
            values.push(ctx, value)?;
        }
        self.union(ctx, &values.data)
    }
}
