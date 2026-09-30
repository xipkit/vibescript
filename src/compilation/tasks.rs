use crate::{Result, budget::Charge};
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
/// ever polled, so native stack use does not grow with nesting. The task
/// stack is bounded by the syntax depth limit, but each suspended task holds
/// a frame of up to a kilobyte or more, so a deeply nested source keeps
/// megabytes of them: each frame is charged to the compilation's memory
/// before its task starts, for as long as it runs.
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

    /// Runs `call`, starting it and every nested call with `start`, and
    /// charging each task's frame to `work` while it runs.
    pub fn run<'a>(
        &self,
        call: C,
        start: impl Fn(C) -> Task<'a, T>,
        work: &dyn super::Work,
    ) -> Result<T> {
        let frame = |task: Task<'a, T>| -> Result<(Task<'a, T>, Option<Charge>)> {
            let held = work.reserve(std::mem::size_of_val(&*task))?;
            Ok((task, held))
        };
        let mut stack = vec![frame(start(call))?];
        let mut context = Context::from_waker(Waker::noop());
        loop {
            let (task, _) = stack.last_mut().unwrap();
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
                    let task = start(call.expect("a suspended task names its nested work"));
                    stack.push(frame(task)?);
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

/// A step a task awaits in a box of its own, since its frame is too large to
/// hold in the task's, with the reservation of that box while it runs.
pub(crate) struct Framed<'a, T> {
    future: Task<'a, T>,
    _held: Option<Charge>,
}

impl<T> Future for Framed<'_, T> {
    type Output = Result<T>;

    fn poll(self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Result<T>> {
        self.get_mut().future.as_mut().poll(context)
    }
}

/// Boxes `future`, a step a task awaits, charging its frame to `work`
/// before it is made.
pub(crate) fn framed<'a, T, F: Future<Output = Result<T>> + 'a>(
    work: &dyn super::Work,
    future: F,
) -> Result<Framed<'a, T>> {
    let held = work.reserve(std::mem::size_of::<F>())?;
    Ok(Framed {
        future: Box::pin(future),
        _held: held,
    })
}
