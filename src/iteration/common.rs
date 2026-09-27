use super::*;

/// Two common nesting levels share one charged allocation until both finish.
pub(crate) struct Pool(Option<Box<State<Slots>>>);

struct Slots {
    drivers: [Option<Driver>; 2],
    length: usize,
}

impl Pool {
    /// Creates a pool without reserving driver storage.
    pub(crate) fn empty() -> Self {
        Self(None)
    }

    /// Returns the number of active drivers.
    pub(crate) fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |slots| slots.length)
    }

    /// Returns the innermost active driver.
    pub(crate) fn last(&self) -> Option<&Driver> {
        let slots = self.0.as_ref()?;
        slots.drivers.get(slots.length.checked_sub(1)?)?.as_ref()
    }

    /// Returns the innermost active driver for advancement.
    pub(crate) fn last_mut(&mut self) -> Option<&mut Driver> {
        let slots = self.0.as_mut()?;
        let index = slots.length.checked_sub(1)?;
        slots.drivers.get_mut(index)?.as_mut()
    }

    pub(super) fn push(&mut self, ctx: &mut CallContext, driver: Driver) -> Result<()> {
        if let Some(slots) = self.0.as_mut() {
            // Reuse keeps the checkpoint the old per-driver reservation made.
            ctx.checkpoint()?;
            let index = slots.length;
            slots.drivers[index] = Some(driver);
            slots.length += 1;
        } else {
            self.0 = Some(boxed(
                ctx,
                Slots {
                    drivers: [Some(driver), None],
                    length: 1,
                },
            )?);
        }
        Ok(())
    }

    /// Drops unwound drivers while retaining the charged allocation for reuse.
    pub(crate) fn truncate(&mut self, length: usize) {
        if let Some(slots) = self.0.as_mut() {
            while slots.length > length {
                slots.length -= 1;
                let index = slots.length;
                slots.drivers[index] = None;
            }
        }
    }
}

/// State for array and numeric iteration, without hash, window or grouping buffers.
pub(crate) struct Driver {
    method: MethodKind,
    receiver: Value,
    position: i128,
    length: i128,
    start: i64,
    stride: i64,
    output: Buffer<Value>,
    accumulator: Option<Value>,
    pending: Value,
    pub(crate) waiting: bool,
}

const _: () = assert!(size_of::<State<Slots>>() <= size_of::<State<Loop>>());
const _: () = assert!(size_of::<Pool>() == size_of::<usize>());

impl Driver {
    pub(super) fn supports(state: &Loop) -> bool {
        use MethodKind::*;
        state.block
            && matches!(
                state.receiver.0,
                Kind::Array(_) | Kind::Range(_) | Kind::Int(_)
            )
            && matches!(
                state.method,
                Each | EachIndex
                    | Map
                    | MapIndex
                    | Select
                    | Reject
                    | Reduce
                    | Times
                    | Upto
                    | Downto
                    | Step
            )
    }

    pub(super) fn from(state: Loop) -> Self {
        Self {
            method: state.method,
            receiver: state.receiver,
            position: state.position,
            length: state.length,
            start: state.start,
            stride: state.stride,
            output: state.output,
            accumulator: state.accumulator,
            pending: Value::nil(),
            waiting: false,
        }
    }

    pub(crate) fn advance(
        &mut self,
        ctx: &mut CallContext,
        returned: Option<Value>,
    ) -> Result<Progress> {
        use MethodKind::*;
        if let Some(value) = returned {
            self.waiting = false;
            match self.method {
                Map | MapIndex => {
                    if value.depth() + 1 > MAX_VALUE_DEPTH {
                        return ctx.guard(ErrorKind::Recursion, "value nesting too deep");
                    }
                    self.output.push(ctx, value)?;
                }
                Select | Reject => {
                    if value.truthy() == (self.method == Select) {
                        self.output.push(ctx, self.pending.clone())?;
                    }
                }
                Reduce => self.accumulator = Some(value),
                _ => (),
            }
        }
        loop {
            ctx.charge(1)?;
            if self.position >= self.length {
                let value = match self.method {
                    Map | MapIndex | Select | Reject => Value::from_array(
                        ctx,
                        std::mem::replace(&mut self.output, Buffer::empty()),
                    )?,
                    Reduce => self.accumulator.take().unwrap_or_default(),
                    _ => self.receiver.clone(),
                };
                return Ok(Progress::Done(value));
            }
            let index = self.position;
            self.position += 1;
            let value = match &self.receiver.0 {
                Kind::Array(array) => array.buffer.data[index as usize].clone(),
                _ => Value::int((i128::from(self.start) + index * i128::from(self.stride)) as i64),
            };
            if matches!(self.method, Select | Reject) {
                self.pending = value.clone();
            }
            let (args, count) = match self.method {
                Reduce => {
                    let Some(accumulator) = self.accumulator.take() else {
                        self.accumulator = Some(value);
                        continue;
                    };
                    ([accumulator, value, Value::nil()], 2)
                }
                EachIndex | MapIndex => ([value, Value::int(index as i64), Value::nil()], 2),
                _ => ([value, Value::nil(), Value::nil()], 1),
            };
            self.waiting = true;
            return Ok(Progress::Yield(args, count));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn pooled_driver_reuses_charged_storage_and_releases_its_values() {
        let mut ctx = CallContext::new(CallOptions::default());
        let receiver = ctx.array(&[Value::int(1), Value::int(2)]).unwrap();
        let mut pool = Pool::empty();
        let state = start_pooled(
            &mut ctx,
            "map",
            &receiver,
            &[],
            &[],
            Some(1),
            Some(&mut pool),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(state, Iteration::Pooled));
        let allocation = std::ptr::from_ref(pool.0.as_deref().unwrap());
        let driver = pool.last_mut().unwrap();
        driver.advance(&mut ctx, None).unwrap();
        let value = ctx.bytes(&[b'x'; 1024]).unwrap();
        driver.advance(&mut ctx, Some(value)).unwrap();
        drop(receiver);
        pool.truncate(0);
        assert_eq!(ctx.stats().retained_memory_bytes, size_of::<State<Slots>>());
        ctx.options.limits.memory_bytes = Some(size_of::<State<Slots>>());
        let next = start_pooled(
            &mut ctx,
            "times",
            &Value::int(1),
            &[],
            &[],
            Some(1),
            Some(&mut pool),
        )
        .unwrap()
        .unwrap();
        assert!(matches!(next, Iteration::Pooled));
        assert_eq!(allocation, std::ptr::from_ref(pool.0.as_deref().unwrap()));
        drop(pool);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn reusing_a_slot_preserves_the_allocation_interruption_boundary() {
        for deadline in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut pool = Pool::empty();
            start_pooled(
                &mut ctx,
                "times",
                &Value::int(1),
                &[],
                &[],
                Some(1),
                Some(&mut pool),
            )
            .unwrap();
            pool.truncate(0);
            if deadline {
                ctx.options.deadline = Some(std::time::Instant::now());
            } else {
                ctx.cancellation().cancel();
            }
            let error = start_pooled(
                &mut ctx,
                "times",
                &Value::int(1),
                &[],
                &[],
                Some(1),
                Some(&mut pool),
            )
            .err()
            .expect("cached storage must not bypass interruption");
            assert_eq!(
                error.kind,
                if deadline {
                    ErrorKind::Deadline
                } else {
                    ErrorKind::Cancelled
                },
            );
            assert_eq!(pool.len(), 0);
            drop(pool);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
