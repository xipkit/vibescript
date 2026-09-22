use super::*;

/// Bounds the cache at 4,096 entries.
const LIMIT: usize = 1 << 12;

/// Identifies a deterministic operation on interned facts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::checking) enum Computation {
    /// A union of two facts, ordered by fact.
    Union(Fact, Fact),
    /// A widening of a previous fact by the next one at a structural depth.
    Widen(Fact, Fact, usize),
    /// A primitive binary operator, by its position in the operator table.
    Binary(u8, Fact, Fact),
    /// A single-selector collection read, and whether it reads stored hash entries.
    Index(Fact, Fact, bool),
    /// The truthiness of a value.
    Truth(Fact),
    /// An indexed collection write of a value.
    Write(Fact, Fact, Fact),
    /// Rendered text appended to interpolated text.
    Append(Fact, Fact),
    /// The ordinary errors a primitive binary operator can raise, by operator position.
    Errors(u8, Fact, Fact),
}

impl Computation {
    fn hash(self) -> u64 {
        let (tag, a, b, c) = match self {
            Self::Union(a, b) => (0, a.0, b.0, 0),
            Self::Widen(a, b, depth) => (1, a.0, b.0, depth),
            Self::Binary(op, a, b) => (2, a.0, b.0, usize::from(op)),
            Self::Index(a, b, stored) => (3, a.0, b.0, usize::from(stored)),
            Self::Truth(a) => (4, a.0, 0, 0),
            Self::Write(a, b, value) => (5, a.0, b.0, value.0),
            Self::Append(a, b) => (6, a.0, b.0, 0),
            Self::Errors(op, a, b) => (7, a.0, b.0, usize::from(op)),
        };
        let hash = (a as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ (b as u64).wrapping_mul(0xc2b2_ae3d_27d4_eb4f)
            ^ (c as u64).wrapping_mul(0x1656_67b1_9e37_79f9)
            ^ tag;
        hash ^ (hash >> 29)
    }
}

/// A direct-mapped, metered cache of deterministic fact operations.
///
/// Loop fixed points, repeated block walks and accumulated operation results repeat the same
/// joins and operators on the same facts many times. Facts are interned and these operations
/// are deterministic, so a remembered result is exactly the fact the operation would build
/// again, and replaying it creates no facts. The table grows with use up to a fixed bound, and
/// a colliding key replaces the older entry.
#[derive(Debug)]
pub(super) struct Memo {
    slots: Buffer<Option<(Computation, Fact, u16)>>,
    stored: usize,
}

impl Memo {
    pub fn new() -> Self {
        Self {
            slots: Buffer::empty(),
            stored: 0,
        }
    }

    fn slot(&self, key: Computation) -> usize {
        key.hash() as usize & (self.slots.data.len() - 1)
    }

    /// Returns a remembered result and its flags.
    pub fn get(&self, ctx: &mut CallContext, key: Computation) -> Result<Option<(Fact, u16)>> {
        ctx.charge(1)?;
        if self.slots.data.is_empty() {
            return Ok(None);
        }
        Ok(match self.slots.data[self.slot(key)] {
            Some((stored, value, flags)) if stored == key => Some((value, flags)),
            _ => None,
        })
    }

    /// Remembers a result and its flags, growing the table while it is under its bound.
    pub fn insert(
        &mut self,
        ctx: &mut CallContext,
        key: Computation,
        (value, flags): (Fact, u16),
    ) -> Result<()> {
        let length = self.slots.data.len();
        if self.stored >= length && length < LIMIT {
            let capacity = (length * 2).max(16);
            let mut slots = Buffer::with_capacity(ctx, capacity)?;
            ctx.charge((capacity + length) as u64)?;
            slots.data.resize(capacity, None);
            let previous = std::mem::replace(&mut self.slots, slots);
            for entry in previous.data.into_iter().flatten() {
                let slot = self.slot(entry.0);
                self.slots.data[slot] = Some(entry);
            }
            self.stored = 0;
        }
        self.stored += 1;
        let slot = self.slot(key);
        self.slots.data[slot] = Some((key, value, flags));
        Ok(())
    }
}

impl Facts {
    /// Returns a remembered operation result and its flags.
    pub(in crate::checking) fn remembered(
        &self,
        ctx: &mut CallContext,
        key: Computation,
    ) -> Result<Option<(Fact, u16)>> {
        self.memo.get(ctx, key)
    }

    /// Remembers an operation result for [`Self::remembered`].
    pub(in crate::checking) fn remember(
        &mut self,
        ctx: &mut CallContext,
        key: Computation,
        result: (Fact, u16),
    ) -> Result<()> {
        self.memo.insert(ctx, key, result)
    }
}
