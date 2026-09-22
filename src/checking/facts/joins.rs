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
        let value = if self.covers(ctx, a, b)? {
            a
        } else if self.covers(ctx, b, a)? {
            b
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
            let general = match self.node(*value) {
                Node::Boolean(_) => Atom::Bool,
                Node::Symbol(_) => Atom::Symbol,
                Node::Integer(_) | Node::IntegerBounds(_) => Atom::Int,
                Node::Float(_) => Atom::Float,
                Node::String(_) => Atom::String,
                Node::Range(..) => Atom::Range,
                Node::Regex(_) => Atom::Regex,
                Node::Atom(Atom::Unknown) => Atom::Any,
                _ => return Ok(false),
            };
            if arms.binary_search(&general.fact()).is_err() {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
