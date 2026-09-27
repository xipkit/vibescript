use super::*;

/// Two common nesting levels share one charged allocation until both finish.
pub(crate) type Pool = Buffer<Driver>;

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

const _: () = assert!(2 * size_of::<State<Driver>>() <= size_of::<State<Loop>>());

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
        let allocation = pool.data.as_ptr();
        let driver = pool.data.last_mut().unwrap();
        driver.advance(&mut ctx, None).unwrap();
        let value = ctx.bytes(&[b'x'; 1024]).unwrap();
        driver.advance(&mut ctx, Some(value)).unwrap();
        drop(receiver);
        pool.data.pop();
        assert_eq!(ctx.stats().retained_memory_bytes, 2 * size_of::<Driver>());
        ctx.options.limits.memory_bytes = Some(2 * size_of::<Driver>());
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
        assert_eq!(allocation, pool.data.as_ptr());
        drop(pool);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn reusing_a_slot_preserves_the_allocation_interruption_boundary() {
        for deadline in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            let mut pool = Pool::empty();
            pool.ensure(&mut ctx, 2).unwrap();
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
            assert!(pool.data.is_empty());
            drop(pool);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }
}
