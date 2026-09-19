//! Optional Tokio hosting for metered script calls and async host capabilities.

use crate::{CallOptions, CancellationToken, Error, ErrorKind, Outcome, Result, Script, Value};
use std::{
    future::{Future, poll_fn},
    panic::{AssertUnwindSafe, catch_unwind},
    pin::{Pin, pin},
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub use crate::vm::asynchronous::AsyncHostCall;

/// A host future that may borrow its scoped handle and arguments until it completes.
pub type HostFuture<'a> = Pin<Box<dyn Future<Output = Result<Value>> + Send + 'a>>;

/// Limits concurrent script workers on the embedding application's Tokio runtime.
#[derive(Clone)]
pub struct Runner {
    slots: Arc<Semaphore>,
}
impl Runner {
    /// Creates a runner with a positive worker limit; it does not create a Tokio runtime.
    pub fn new(workers: usize) -> Result<Self> {
        if workers == 0 || workers > Semaphore::MAX_PERMITS {
            return Err(Error::new(ErrorKind::Argument, "invalid worker count"));
        }
        Ok(Self {
            slots: Arc::new(Semaphore::new(workers)),
        })
    }
    /// Reports slots not currently held by queued or executing blocking jobs.
    pub fn available_slots(&self) -> usize {
        self.slots.available_permits()
    }
    /// Runs script work on bounded workers and awaits native host futures without a worker.
    ///
    /// Dropping the future requests cancellation. A synchronous host callback keeps
    /// its worker reservation until it returns, including during nested async block
    /// calls. Cancellation, deadlines and latched quotas interrupt async host waits.
    /// Trusted callbacks must keep individual future polls bounded and cooperate
    /// with cancellation while doing synchronous work.
    pub async fn call(
        &self,
        script: Script,
        name: String,
        args: Vec<Value>,
        options: CallOptions,
    ) -> Result<Outcome> {
        self.call_with_keywords(script, name, args, Vec::new(), options)
            .await
    }
    /// Runs a call with named arguments under the same worker and cancellation limits.
    pub async fn call_with_keywords(
        &self,
        script: Script,
        name: String,
        args: Vec<Value>,
        keywords: Vec<(String, Value)>,
        mut options: CallOptions,
    ) -> Result<Outcome> {
        options.cancellation = options.cancellation.child_token();
        let _guard = CancelOnDrop(options.cancellation.clone());
        let mut future = pin!(crate::vm::asynchronous::call(
            self.slots.clone(),
            script,
            name,
            args,
            keywords,
            options,
        ));
        let result =
            poll_fn(
                |cx| match catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(cx))) {
                    Ok(Poll::Ready(result)) => Poll::Ready(Ok(result)),
                    Ok(Poll::Pending) => Poll::Pending,
                    Err(panic) => Poll::Ready(Err(panic)),
                },
            )
            .await;
        match result {
            Ok(result) => result,
            Err(panic) => {
                let message = panic
                    .downcast_ref::<String>()
                    .map(String::as_str)
                    .or_else(|| panic.downcast_ref::<&str>().copied())
                    .unwrap_or("host callback panic");
                Err(Error::new(
                    ErrorKind::Host,
                    format!("script worker failed: {message}"),
                ))
            }
        }
    }
}

pub(crate) async fn acquire(
    slots: Arc<Semaphore>,
    cancellation: &CancellationToken,
    deadline: Option<Instant>,
) -> Result<OwnedSemaphorePermit> {
    let acquire = slots.acquire_owned();
    tokio::pin!(acquire);
    loop {
        if cancellation.is_cancelled() {
            return Err(Error::new(ErrorKind::Cancelled, "execution cancelled"));
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(Error::new(
                ErrorKind::Deadline,
                "execution deadline exceeded",
            ));
        }
        tokio::select! {
            permit = &mut acquire => return permit.map_err(|_| Error::new(ErrorKind::Host, "worker pool closed")),
            _ = tokio::time::sleep(Duration::from_millis(5)) => (),
        }
    }
}

struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
