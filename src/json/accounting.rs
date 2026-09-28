use crate::{
    CallContext, ErrorKind, Result, Value,
    budget::{CHUNK, Charge},
    value::{Bytes, Kind},
};
use std::ops::{Deref, DerefMut};

/// Selects ordinary or reserved accounting without a per-operation branch.
pub(super) trait Context: Deref<Target = CallContext> + DerefMut {
    /// Whether token copies may reserve memory credit.
    const BATCHED: bool;

    /// Charges work at its original logical position.
    fn charge(&mut self, steps: u64) -> Result<()>;
    /// Borrows a phase that already settled any pending steps.
    fn settled(&mut self) -> &mut CallContext;

    #[cfg(test)]
    /// Selects the original accounting operations for differential tests.
    fn set_unbatched(&mut self, unbatched: bool);
}

impl Context for &mut CallContext {
    const BATCHED: bool = false;

    #[inline]
    fn charge(&mut self, steps: u64) -> Result<()> {
        CallContext::charge(self, steps)
    }

    #[inline]
    fn settled(&mut self) -> &mut CallContext {
        self
    }

    #[cfg(test)]
    fn set_unbatched(&mut self, _: bool) {}
}

/// A bounded reservation of steps before the next quota/checkpoint boundary.
/// Mutable `CallContext` calls settle first, including allocations, guards and
/// explicit checkpoints. A draw that reaches a boundary uses `charge` itself,
/// preserving multi-step overshoot, error precedence and interruption checks.
pub(super) struct Steps<'a> {
    ctx: &'a mut CallContext,
    pending: u64,
    available: u64,
    maximum: u64,
    #[cfg(test)]
    pub unbatched: bool,
}

impl<'a> Steps<'a> {
    /// Bounds local work by the input and the existing quota/checkpoint limits.
    pub fn new(ctx: &'a mut CallContext, maximum: usize) -> Self {
        Self {
            ctx,
            pending: 0,
            available: 0,
            maximum: maximum.min(CHUNK) as u64,
            #[cfg(test)]
            unbatched: false,
        }
    }

    /// Borrows a phase that already settled its allowance and batches its own
    /// work, such as string unescaping. No reservation may survive this borrow.
    #[inline]
    pub fn settled(&mut self) -> &mut CallContext {
        debug_assert_eq!((self.pending, self.available), (0, 0));
        self.ctx
    }

    /// Charges at the original logical point, drawing only before a boundary.
    #[inline]
    pub fn charge(&mut self, steps: u64) -> Result<()> {
        #[cfg(test)]
        if self.unbatched {
            return self.ctx.charge(steps);
        }
        if steps > self.available || self.available == 0 {
            self.settle();
            self.available = self.ctx.step_allowance(self.maximum);
            if steps > self.available || self.available == 0 {
                self.available = 0;
                return self.ctx.charge(steps);
            }
        }
        self.available -= steps;
        self.pending += steps;
        Ok(())
    }

    #[inline]
    fn settle(&mut self) {
        if self.pending != 0 {
            // No context access can cross this reservation: DerefMut settles
            // first, and its allowance excluded both quota and checkpoint.
            self.ctx
                .settle_step_allowance(std::mem::take(&mut self.pending));
        }
        self.available = 0;
    }
}

impl Context for Steps<'_> {
    const BATCHED: bool = true;

    #[inline]
    fn charge(&mut self, steps: u64) -> Result<()> {
        Steps::charge(self, steps)
    }

    #[inline]
    fn settled(&mut self) -> &mut CallContext {
        Steps::settled(self)
    }

    #[cfg(test)]
    fn set_unbatched(&mut self, unbatched: bool) {
        self.unbatched = unbatched;
    }
}

impl Deref for Steps<'_> {
    type Target = CallContext;

    fn deref(&self) -> &Self::Target {
        self.ctx
    }
}

impl DerefMut for Steps<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.settle();
        self.ctx
    }
}

impl Drop for Steps<'_> {
    fn drop(&mut self) {
        self.settle();
    }
}

/// A token-sized reservation. Unused credit is invisible to the peak and is
/// settled before any other allocator can inspect the ledger. Each draw keeps
/// the old checkpoint; insufficient credit falls back at that exact draw,
/// after the same preceding steps, rather than failing when credit is requested.
struct Reservation {
    charge: Option<Charge>,
    unused: usize,
    peak: usize,
}

impl Reservation {
    fn new(ctx: &mut CallContext, maximum: usize) -> Self {
        let charge = ctx.reserve_available(maximum);
        let unused = charge.bytes();
        Self {
            charge: Some(charge),
            unused,
            peak: 0,
        }
    }

    #[inline]
    fn draw(&mut self, ctx: &mut CallContext, bytes: usize) -> Result<()> {
        ctx.checkpoint()?;
        if bytes > self.unused {
            self.settle();
            let charge = ctx.reserve(bytes)?;
            Charge::merge(&mut self.charge, charge);
        } else {
            self.unused -= bytes;
            self.peak = self
                .peak
                .max(self.charge.as_ref().unwrap().reserved_usage(self.unused));
        }
        Ok(())
    }

    fn settle(&mut self) {
        if self.peak != 0 {
            self.charge
                .as_ref()
                .unwrap()
                .publish_peak(std::mem::take(&mut self.peak));
        }
        if self.unused != 0 {
            self.charge.as_mut().unwrap().release(self.unused);
            self.unused = 0;
        }
    }

    fn finish(mut self) -> Option<Charge> {
        self.settle();
        self.charge.take()
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.settle();
    }
}

/// Copies one scanned token with a single ledger reservation for its backing
/// bytes and value header. Logical work and checkpoints follow
/// `Buffer::with_capacity`, `extend`, then `Bytes::new`. The scope cannot call
/// host code: it accumulates the same live peaks locally and publishes before
/// another allocation, return or error, avoiding per-draw atomic peak updates.
pub(super) fn copy_bytes(ctx: &mut CallContext, input: &[u8]) -> Result<Value> {
    let mut reservation = Reservation::new(ctx, input.len().saturating_add(Bytes::header_bytes()));
    let mut data = Vec::new();
    if !input.is_empty() {
        reservation.draw(ctx, input.len())?;
        if data.try_reserve_exact(input.len()).is_err() {
            return ctx.fail(ErrorKind::Memory, "allocation failed");
        }
        // The supported allocators report the requested capacity. Preserve
        // accounting even if an allocator supplies a larger one.
        if data.capacity() != input.len() {
            reservation.settle();
            let replacement = ctx.reserve(data.capacity())?;
            reservation.charge = replacement;
        }
    }
    for chunk in input.chunks(CHUNK) {
        ctx.work_bytes(chunk.len())?;
        data.extend_from_slice(chunk);
    }
    reservation.draw(ctx, Bytes::header_bytes())?;
    Ok(Value(Kind::Bytes(Bytes::from_parts(
        data,
        None,
        reservation.finish(),
    ))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallOptions, Limits};

    #[test]
    fn step_reservations_keep_quota_cancellation_and_deadline_checkpoints() {
        for before in 0..33 {
            for limit in 0..65 {
                for interrupt in 0..6 {
                    for deadline in [false, true] {
                        let run = |unbatched| {
                            let mut ctx = CallContext::new(CallOptions::default());
                            ctx.charge(before).unwrap();
                            ctx.options.limits.steps = Some(limit);
                            let token = ctx.cancellation().clone();
                            if deadline {
                                ctx.options.deadline = Some(std::time::Instant::now());
                            }
                            let mut steps = Steps::new(&mut ctx, CHUNK);
                            steps.unbatched = unbatched;
                            let result = [1, 2, 13, 0, 32, 1].into_iter().enumerate().try_for_each(
                                |(index, n)| {
                                    if index == interrupt {
                                        token.cancel();
                                    }
                                    steps.charge(n)
                                },
                            );
                            drop(steps);
                            (result, ctx.stats().steps, ctx.checkpoint())
                        };
                        assert_eq!(
                            run(false),
                            run(true),
                            "{before} {limit} {interrupt} {deadline}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn reservations_preserve_every_string_quota_boundary() {
        for input in [b"".as_slice(), b"a", b"ordinary", &[b'x'; CHUNK + 1]] {
            let run = |batched, steps, memory| {
                let mut ctx = CallContext::new(CallOptions {
                    limits: Limits {
                        steps,
                        memory_bytes: memory,
                        ..Limits::default()
                    },
                    ..CallOptions::default()
                });
                let value = if batched {
                    copy_bytes(&mut ctx, input)
                } else {
                    ctx.bytes(input)
                };
                let stats = ctx.stats();
                let outcome = value.map(|value| value.as_bytes().unwrap().to_vec());
                assert_eq!(ctx.stats().retained_memory_bytes, 0);
                (
                    outcome,
                    stats.steps,
                    stats.peak_memory_bytes,
                    stats.retained_memory_bytes,
                    ctx.checkpoint(),
                )
            };
            let baseline = run(false, None, None);
            assert_eq!(run(true, None, None), baseline);
            for steps in 0..=baseline.1 + 1 {
                for memory in 0..=baseline.2 + 1 {
                    assert_eq!(
                        run(true, Some(steps), Some(memory)),
                        run(false, Some(steps), Some(memory)),
                        "{} bytes, steps {steps}, memory {memory}",
                        input.len()
                    );
                }
            }
        }
    }
}
