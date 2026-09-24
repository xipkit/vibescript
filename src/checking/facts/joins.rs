use super::{memo::Computation, *};

impl Facts {
    /// Joins two facts, the common case of [`Self::union`].
    ///
    /// A side that already covers the other is the union. Otherwise both sorted alternative
    /// lists merge instead of sorting their concatenation.
    pub(super) fn pair(&mut self, ctx: &mut CallContext, a: Fact, b: Fact) -> Result<Fact> {
        if a == b || b == Atom::Never.fact() {
            return Ok(a);
        }
        if a == Atom::Never.fact() {
            return Ok(b);
        }
        // Unions are commutative, so one ordered key serves both argument orders.
        let (a, b) = (a.min(b), a.max(b));
        if let Some((value, _)) = self.memo.get(ctx, Computation::Union(a, b))? {
            return Ok(value);
        }
        let (left, right) = (self.arms(&a).len(), self.arms(&b).len());
        let search = |n: usize| n.ilog2() as usize + 1;
        // Searching suits a small side joining a large union; comparable unions merge instead.
        let covering = if left.saturating_mul(search(right)) + right.saturating_mul(search(left))
            <= left + right
        {
            if self.covers(ctx, a, b)? {
                Some(a)
            } else if self.covers(ctx, b, a)? {
                Some(b)
            } else {
                None
            }
        } else {
            self.covering(ctx, a, b)?
        };
        let value = if let Some(value) = covering {
            value
        } else {
            let (left, right) = (self.arms(&a), self.arms(&b));
            ctx.charge((left.len() + right.len()) as u64)?;
            let mut arms = Buffer::with_capacity(ctx, left.len() + right.len())?;
            let (mut i, mut j) = (0, 0);
            while i < left.len() || j < right.len() {
                let next = match (left.get(i), right.get(j)) {
                    (Some(&x), Some(&y)) if x <= y => {
                        i += 1;
                        j += usize::from(x == y);
                        x
                    }
                    (_, Some(&y)) => {
                        j += 1;
                        y
                    }
                    (Some(&x), None) => {
                        i += 1;
                        x
                    }
                    (None, None) => unreachable!(),
                };
                arms.data.push(next);
            }
            self.union_of(ctx, arms)?
        };
        self.memo
            .insert(ctx, Computation::Union(a, b), (value, 0))?;
        Ok(value)
    }

    /// Returns the sorted alternatives of a fact other than `never`.
    fn arms<'a>(&'a self, value: &'a Fact) -> &'a [Fact] {
        match self.node(*value) {
            Node::Union(arms) => &arms.data,
            _ => std::slice::from_ref(value),
        }
    }

    /// Reports whether joining `value` into `into` leaves `into` unchanged: every alternative
    /// of `value` is already present or absorbed by a general scalar, as normalization does.
    fn covers(&self, ctx: &mut CallContext, into: Fact, value: Fact) -> Result<bool> {
        let (arms, values) = (self.arms(&into), self.arms(&value));
        let search = u64::from(arms.len().ilog2() + 1);
        for value in values {
            ctx.charge(search)?;
            if arms.binary_search(value).is_ok() {
                continue;
            }
            let Some(general) = self.general(*value) else {
                return Ok(false);
            };
            if arms.binary_search(&general.fact()).is_err() {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Returns the side that covers the other, as [`Self::covers`] checks in that order, by
    /// merging both sorted alternative lists once.
    fn covering(&self, ctx: &mut CallContext, a: Fact, b: Fact) -> Result<Option<Fact>> {
        let (left, right) = (self.arms(&a), self.arms(&b));
        ctx.charge((left.len() + right.len()) as u64)?;
        let atoms = |arms: &[Fact]| {
            arms.iter()
                .take_while(|fact| fact.0 <= Atom::Regex as usize)
                .fold(0u16, |mask, fact| mask | 1 << fact.0)
        };
        let (left_atoms, right_atoms) = (atoms(left), atoms(right));
        let absorbed = |value: Fact, atoms: u16| {
            self.general(value)
                .is_some_and(|general| atoms & 1 << general as usize != 0)
        };
        let (mut left_covers, mut right_covers) = (true, true);
        let (mut i, mut j) = (0, 0);
        while (left_covers || right_covers) && (i < left.len() || j < right.len()) {
            match (left.get(i), right.get(j)) {
                (Some(&x), Some(&y)) if x == y => {
                    i += 1;
                    j += 1;
                }
                (Some(&x), Some(&y)) if x < y => {
                    right_covers &= absorbed(x, right_atoms);
                    i += 1;
                }
                (Some(&x), None) => {
                    right_covers &= absorbed(x, right_atoms);
                    i += 1;
                }
                (_, Some(&y)) => {
                    left_covers &= absorbed(y, left_atoms);
                    j += 1;
                }
                (None, None) => unreachable!(),
            }
        }
        Ok(if left_covers {
            Some(a)
        } else if right_covers {
            Some(b)
        } else {
            None
        })
    }

    /// Returns the general scalar that absorbs a literal alternative during normalization.
    fn general(&self, value: Fact) -> Option<Atom> {
        Some(match self.node(value) {
            Node::Boolean(_) => Atom::Bool,
            Node::Symbol(_) => Atom::Symbol,
            Node::Integer(_) | Node::IntegerBounds(_) => Atom::Int,
            Node::Float(_) => Atom::Float,
            Node::String(_) => Atom::String,
            Node::Range(..) => Atom::Range,
            Node::Regex(_) => Atom::Regex,
            Node::Atom(Atom::Unknown) => Atom::Any,
            _ => return None,
        })
    }
}

/// The literal alternatives of one scalar kind that a branch join keeps.
const LITERALS: usize = 64;

impl Facts {
    /// Bounds the literal alternatives a branch join accumulates.
    ///
    /// Past [`LITERALS`] alternatives of one kind, integer literals and ranges become the range
    /// that spans them, and other literals become their general scalar, as the Go checker
    /// widens static values. Without a bound, a binding assigned a different literal on each of
    /// N branches costs every later join and operation on it N steps.
    pub(in crate::checking) fn bound_literals(
        &mut self,
        ctx: &mut CallContext,
        value: Fact,
    ) -> Result<Fact> {
        let arms = self.arms(&value);
        if arms.len() <= LITERALS {
            return Ok(value);
        }
        ctx.charge(arms.len() as u64)?;
        let mut counts = [0usize; 4];
        let mut hull = super::super::integers::Bounds {
            min: Some(i64::MAX),
            max: Some(i64::MIN),
        };
        for &arm in arms {
            let (min, max) = match self.node(arm) {
                Node::Integer(value) => (Some(*value), Some(*value)),
                Node::IntegerBounds(bounds) => (bounds.min, bounds.max),
                Node::String(_) => {
                    counts[1] += 1;
                    continue;
                }
                Node::Symbol(_) => {
                    counts[2] += 1;
                    continue;
                }
                Node::Float(_) => {
                    counts[3] += 1;
                    continue;
                }
                _ => continue,
            };
            counts[0] += 1;
            hull.min = hull.min.zip(min).map(|(a, b)| a.min(b));
            hull.max = hull.max.zip(max).map(|(a, b)| a.max(b));
        }
        let widened = counts.map(|count| count > LITERALS);
        if !widened.contains(&true) {
            return Ok(value);
        }
        let mut kept = Buffer::with_capacity(ctx, arms.len())?;
        for &arm in self.arms(&value) {
            let kind = match self.node(arm) {
                Node::Integer(_) | Node::IntegerBounds(_) => 0,
                Node::String(_) => 1,
                Node::Symbol(_) => 2,
                Node::Float(_) => 3,
                _ => {
                    kept.data.push(arm);
                    continue;
                }
            };
            if !widened[kind] {
                kept.data.push(arm);
            }
        }
        if widened[0] {
            let range = self.integer_range(ctx, hull)?;
            kept.push(ctx, range)?;
        }
        for (kind, atom) in [(1, Atom::String), (2, Atom::Symbol), (3, Atom::Float)] {
            if widened[kind] {
                kept.push(ctx, atom.fact())?;
            }
        }
        self.union(ctx, &kept.data)
    }
}
