//! Explicit, budget-charged frame stacks for container rendering walks.
//!
//! The rendering walks over arrays and hashes (`to_s` and interpolation,
//! bounded projection and output, `inspect`) keep one frame per open container
//! here instead of on the native stack. Nesting depth is therefore bounded by
//! the memory quota and the value depth guard, never by the thread stack size.
//! Frames live in a [`Buffer`], so every push is charged and every drop, whether
//! on completion, early stopping or error, releases the scratch immediately.

use crate::{CallContext, ErrorKind, Result, Value, budget::Buffer};

/// The entries of an open container, borrowed from the value being rendered.
#[derive(Clone, Copy)]
pub(super) enum Entries<'a> {
    Array(&'a [Value]),
    Hash(&'a [(Value, Value)]),
}

impl<'a> Entries<'a> {
    pub fn len(self) -> usize {
        match self {
            Self::Array(values) => values.len(),
            Self::Hash(entries) => entries.len(),
        }
    }

    /// Returns the delimiter that closes this container.
    pub fn closing(self) -> &'static [u8] {
        match self {
            Self::Array(_) => b"]",
            Self::Hash(_) => b"}",
        }
    }

    /// Returns the hash key, if any, and the value stored at `index`.
    pub fn get(self, index: usize) -> (Option<&'a Value>, &'a Value) {
        match self {
            Self::Array(values) => (None, &values[index]),
            Self::Hash(entries) => {
                let (key, value) = &entries[index];
                (Some(key), value)
            }
        }
    }
}

/// One open container plus walker-specific state such as an object field order.
pub(super) struct Frame<'a, X> {
    pub entries: Entries<'a>,
    position: usize,
    pub extra: X,
}

impl<'a, X> Frame<'a, X> {
    pub fn new(entries: Entries<'a>, extra: X) -> Self {
        Self {
            entries,
            position: 0,
            extra,
        }
    }

    /// Claims the next position in this container, or `None` once it is exhausted.
    pub fn next(&mut self) -> Option<usize> {
        if self.position < self.entries.len() {
            self.position += 1;
            Some(self.position - 1)
        } else {
            None
        }
    }
}

/// A metered stack of open containers. Its depth is the number of enclosing containers.
pub(super) struct Stack<'a, X> {
    frames: Buffer<Frame<'a, X>>,
}

impl<'a, X> Stack<'a, X> {
    /// Creates an empty stack without allocating; scalar roots never touch the budget.
    pub fn new() -> Self {
        Self {
            frames: Buffer::empty(),
        }
    }

    pub fn depth(&self) -> usize {
        self.frames.data.len()
    }

    /// Pushes a frame, doubling the charged capacity from two frames upwards.
    pub fn push(&mut self, ctx: &mut CallContext, frame: Frame<'a, X>) -> Result<()> {
        let data = &self.frames.data;
        if data.len() == data.capacity() {
            let Some(capacity) = data.capacity().max(1).checked_mul(2) else {
                return ctx.fail(ErrorKind::Memory, "allocation size overflow");
            };
            self.frames.ensure(ctx, capacity)?;
        }
        self.frames.data.push(frame);
        Ok(())
    }

    pub fn top(&mut self) -> Option<&mut Frame<'a, X>> {
        self.frames.data.last_mut()
    }

    /// Pops the innermost container, releasing any scratch owned by its frame.
    pub fn pop(&mut self) -> Option<Frame<'a, X>> {
        self.frames.data.pop()
    }
}

#[cfg(test)]
pub(super) mod support {
    use super::*;
    use std::mem::size_of;

    /// Charged frame capacity after `depth` frames were pushed.
    pub fn capacity(depth: usize) -> usize {
        if depth == 0 {
            0
        } else {
            depth.max(2).next_power_of_two()
        }
    }

    /// Bytes charged for the stack while it holds `depth` frames.
    pub fn bytes<X>(depth: usize) -> usize {
        capacity(depth) * size_of::<Frame<'static, X>>()
    }

    /// Peak bytes charged for a stack that reached `depth` frames, including the
    /// previous allocation held while the buffer grows.
    pub fn peak<X>(depth: usize) -> usize {
        let capacity = capacity(depth);
        let previous = if capacity > 2 { capacity / 2 } else { 0 };
        (capacity + previous) * size_of::<Frame<'static, X>>()
    }

    /// Builds `depth` nested host arrays around `leaf` without touching a budget.
    pub fn nested_arrays(depth: usize, leaf: Value) -> Value {
        let mut value = leaf;
        for _ in 0..depth {
            value = Value::array(vec![value]);
        }
        value
    }

    /// Builds `depth` nested host hashes keyed `k` around `leaf`.
    pub fn nested_hashes(depth: usize, leaf: Value) -> Value {
        let mut value = leaf;
        for _ in 0..depth {
            value = Value::hash(vec![(b"k".to_vec(), value)]);
        }
        value
    }

    /// Runs `body` on a thread whose stack is far too small for a recursive walk
    /// over ten thousand containers, so any leftover recursion aborts loudly.
    pub fn on_small_stack<T: Send + 'static>(body: impl FnOnce() -> T + Send + 'static) -> T {
        std::thread::Builder::new()
            .stack_size(192 << 10)
            .spawn(body)
            .unwrap()
            .join()
            .unwrap()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CallOptions;

    #[test]
    fn frames_are_charged_while_open_and_released_when_popped() {
        let mut ctx = CallContext::new(CallOptions::default());
        let values = [Value::int(1), Value::int(2)];
        let mut stack: Stack<'_, ()> = Stack::new();
        assert_eq!(ctx.stats().peak_memory_bytes, 0);
        for depth in 1..=5 {
            stack
                .push(&mut ctx, Frame::new(Entries::Array(&values), ()))
                .unwrap();
            assert_eq!(stack.depth(), depth);
            assert_eq!(
                ctx.stats().retained_memory_bytes,
                support::bytes::<()>(depth)
            );
            assert_eq!(ctx.stats().peak_memory_bytes, support::peak::<()>(depth));
        }
        let frame = stack.top().unwrap();
        assert_eq!(frame.next(), Some(0));
        assert_eq!(frame.next(), Some(1));
        assert_eq!(frame.next(), None);
        assert_eq!(frame.next(), None);
        let (key, value) = frame.entries.get(1);
        assert!(key.is_none());
        assert_eq!(value.as_int(), Some(2));
        while stack.pop().is_some() {}
        assert_eq!(ctx.stats().retained_memory_bytes, support::bytes::<()>(5));
        drop(stack);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn frame_scratch_is_released_when_a_push_is_refused() {
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.memory_bytes = Some(support::bytes::<()>(2));
        let values = [Value::nil()];
        let mut stack: Stack<'_, ()> = Stack::new();
        stack
            .push(&mut ctx, Frame::new(Entries::Array(&values), ()))
            .unwrap();
        stack
            .push(&mut ctx, Frame::new(Entries::Array(&values), ()))
            .unwrap();
        let error = stack
            .push(&mut ctx, Frame::new(Entries::Array(&values), ()))
            .unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(stack.depth(), 2);
        drop(stack);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.charge(1).unwrap_err().kind, ErrorKind::Memory);
    }

    #[test]
    fn extra_frame_state_lives_exactly_as_long_as_its_frame() {
        let mut ctx = CallContext::new(CallOptions::default());
        let entries = [(Value::bytes(b"k"), Value::nil())];
        let order: Buffer<usize> = Buffer::with_capacity(&mut ctx, 8).unwrap();
        let mut stack: Stack<'_, Option<Buffer<usize>>> = Stack::new();
        stack
            .push(&mut ctx, Frame::new(Entries::Hash(&entries), Some(order)))
            .unwrap();
        let scratch = support::bytes::<Option<Buffer<usize>>>(1);
        let order = size_of::<[usize; 8]>();
        assert_eq!(ctx.stats().retained_memory_bytes, scratch + order);
        let (key, _) = stack.top().unwrap().entries.get(0);
        assert_eq!(key.unwrap().as_bytes(), Some(b"k".as_slice()));
        assert_eq!(stack.top().unwrap().entries.closing(), b"}");
        drop(stack.pop());
        assert_eq!(ctx.stats().retained_memory_bytes, scratch);
        drop(stack);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }
}
