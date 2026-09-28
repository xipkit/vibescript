use crate::Result;
use std::{
    cell::{Cell, RefCell},
    future::{Future, poll_fn},
    pin::Pin,
    task::{Context, Poll, Waker},
};

/// A suspended piece of nested compiler work.
pub(crate) type Task<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + 'a>>;

/// Runs recursive compiler passes on a heap stack instead of the native stack.
///
/// Source nesting is bounded by the syntax depth limit rather than by the
/// size of the thread that compiles it. A pass writes each recursive step as
/// an async task. A task that needs nested work names it with [`Tasks::call`]
/// and suspends; [`Tasks::run`] then starts the nested task on top of its
/// stack and resumes the caller with the result. Only the innermost task is
/// ever polled, so native stack use does not grow with nesting. Like the
/// native stack it replaces, the task stack is bounded by the syntax depth
/// limit and is not charged to compilation budgets.
pub(crate) struct Tasks<C, T> {
    call: Cell<Option<C>>,
    result: RefCell<Option<Result<T>>>,
}

impl<C, T> Tasks<C, T> {
    pub fn new() -> Self {
        Self {
            call: Cell::new(None),
            result: RefCell::new(None),
        }
    }

    /// Runs `call`, starting it and every nested call with `start`.
    pub fn run<'a>(&self, call: C, start: impl Fn(C) -> Task<'a, T>) -> Result<T> {
        let mut stack = vec![start(call)];
        let mut context = Context::from_waker(Waker::noop());
        loop {
            let task = stack.last_mut().unwrap();
            match task.as_mut().poll(&mut context) {
                Poll::Ready(result) => {
                    stack.pop();
                    if stack.is_empty() {
                        return result;
                    }
                    *self.result.borrow_mut() = Some(result);
                }
                Poll::Pending => {
                    let call = self.call.take();
                    stack.push(start(call.expect("a suspended task names its nested work")));
                }
            }
        }
    }

    /// Suspends the calling task until the nested `call` finishes.
    pub async fn call(&self, call: C) -> Result<T> {
        self.call.set(Some(call));
        let mut suspended = false;
        poll_fn(|_| {
            if std::mem::replace(&mut suspended, true) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
        self.result
            .borrow_mut()
            .take()
            .expect("nested work finishes before its caller resumes")
    }
}
