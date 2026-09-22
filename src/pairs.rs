use crate::{
    CallContext, ErrorKind, Result,
    budget::{Buffer, CHUNK},
};
use std::mem::size_of;

/// Tables up to this many slots are cleared in place; larger ones are released.
const RETAINED_SLOTS: usize = 64;

/// Container pairs that one structural comparison has finished, keyed by the
/// storage addresses of both sides.
///
/// Values are immutable and stay borrowed while a comparison runs, so an
/// address pair names the same two containers until the table is cleared.
/// Shared subtrees are then compared once per pair rather than once per path
/// that reaches them. Insertions and growth are charged. Probes are expected
/// constant time and uncharged, because addresses, unlike the number of
/// entries, vary between runs.
pub(crate) struct Pairs<T> {
    slots: Buffer<(usize, usize, T)>,
    len: usize,
}

impl<T: Copy + Default> Pairs<T> {
    pub fn new() -> Self {
        Self {
            slots: Buffer::empty(),
            len: 0,
        }
    }

    fn hash(left: usize, right: usize) -> usize {
        let mut hash =
            (left as u64).wrapping_mul(0x9e3779b97f4a7c15) ^ (right as u64).rotate_left(27);
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51afd7ed558ccd);
        (hash ^ (hash >> 33)) as usize
    }

    /// Returns the slot holding the pair, or the vacant slot it would occupy.
    fn slot(&self, left: usize, right: usize) -> usize {
        let mask = self.slots.data.len() - 1;
        let mut slot = Self::hash(left, right) & mask;
        loop {
            let (a, b, _) = self.slots.data[slot];
            if a == 0 || (a == left && b == right) {
                return slot;
            }
            slot = (slot + 1) & mask;
        }
    }

    /// Returns the result recorded for a pair of nonzero addresses.
    pub fn get(&self, left: usize, right: usize) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let (a, _, value) = self.slots.data[self.slot(left, right)];
        (a != 0).then_some(value)
    }

    /// Records the result for a pair of nonzero addresses not yet recorded.
    pub fn insert(
        &mut self,
        ctx: &mut CallContext,
        left: usize,
        right: usize,
        value: T,
    ) -> Result<()> {
        debug_assert!(left != 0 && self.get(left, right).is_none());
        ctx.charge(1)?;
        if 2 * (self.len + 1) > self.slots.data.len() {
            let Some(size) = self.slots.data.len().checked_mul(2).map(|n| n.max(8)) else {
                return ctx.fail(ErrorKind::Memory, "comparison memo size overflow");
            };
            let mut slots = Buffer::with_capacity(ctx, size)?;
            let empty = (0, 0, T::default());
            while slots.data.len() < size {
                let end = size.min(slots.data.len() + CHUNK / size_of::<(usize, usize, T)>());
                ctx.work_bytes((end - slots.data.len()) * size_of::<(usize, usize, T)>())?;
                slots.data.resize(end, empty);
            }
            let old = std::mem::replace(&mut self.slots, slots);
            for &(a, b, value) in &old.data {
                if a != 0 {
                    ctx.charge(1)?;
                    let slot = self.slot(a, b);
                    self.slots.data[slot] = (a, b, value);
                }
            }
        }
        let slot = self.slot(left, right);
        self.slots.data[slot] = (left, right, value);
        self.len += 1;
        Ok(())
    }

    /// Forgets every pair before the compared values may be released.
    pub fn clear(&mut self, ctx: &mut CallContext) -> Result<()> {
        if self.len == 0 {
            return Ok(());
        }
        self.len = 0;
        if self.slots.data.len() > RETAINED_SLOTS {
            self.slots = Buffer::empty();
            return Ok(());
        }
        ctx.work_bytes(std::mem::size_of_val(self.slots.data.as_slice()))?;
        self.slots.data.fill((0, 0, T::default()));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn growth_and_clearing_are_charged_by_entry_count_alone() {
        let mut counters = Vec::new();
        // Colliding and spread addresses cost the same work and storage.
        for step in [1usize << 20, 8] {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut pairs = Pairs::new();
            for i in 1..=1000 {
                pairs.insert(&mut ctx, i * step, 7, i).unwrap();
            }
            for i in 1..=1000 {
                assert_eq!(pairs.get(i * step, 7), Some(i));
                assert_eq!(pairs.get(i * step, 8), None);
            }
            assert!(ctx.stats().retained_memory_bytes >= 2048 * 24);
            pairs.clear(&mut ctx).unwrap();
            assert_eq!(pairs.get(step, 7), None);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
            pairs.insert(&mut ctx, step, 7, 1).unwrap();
            pairs.clear(&mut ctx).unwrap();
            assert_eq!(pairs.get(step, 7), None);
            drop(pairs);
            let stats = ctx.stats();
            assert_eq!(stats.retained_memory_bytes, 0);
            counters.push((stats.steps, stats.peak_memory_bytes));
        }
        assert_eq!(counters[0], counters[1]);
    }

    #[test]
    fn insertion_observes_quotas() {
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.memory_bytes = Some(8 * size_of::<(usize, usize, ())>());
        let mut pairs = Pairs::new();
        for i in 1..=4 {
            pairs.insert(&mut ctx, i, 1, ()).unwrap();
        }
        // A fifth pair needs sixteen slots.
        assert_eq!(
            pairs.insert(&mut ctx, 5, 1, ()).unwrap_err().kind,
            ErrorKind::Memory
        );
        drop(pairs);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = Some(10);
        let mut pairs = Pairs::new();
        let error = (1..=100)
            .map(|i| pairs.insert(&mut ctx, i, 1, ()))
            .find_map(Result::err)
            .unwrap();
        assert_eq!(error.kind, ErrorKind::Steps);
        assert_eq!(ctx.checkpoint().unwrap_err().kind, ErrorKind::Steps);
    }
}
