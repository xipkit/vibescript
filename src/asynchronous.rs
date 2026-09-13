//! Optional Tokio hosting for CPU-bound script calls. Host callbacks remain synchronous.

use crate::{CallOptions, CancellationToken, Error, ErrorKind, Outcome, Result, Script, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;

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
    /// Runs a call on a bounded blocking worker. Dropping the future requests cancellation.
    ///
    /// The worker holds its permit until it actually exits, including when a trusted host
    /// callback is slow to observe cancellation. Queued calls poll cancellation every 5 ms.
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
        let guard = CancelOnDrop(options.cancellation.clone());
        let acquire = self.slots.clone().acquire_owned();
        tokio::pin!(acquire);
        let permit = loop {
            if options.cancellation.is_cancelled() {
                return Err(Error::new(ErrorKind::Cancelled, "execution cancelled"));
            }
            if options.deadline.is_some_and(|d| Instant::now() >= d) {
                return Err(Error::new(
                    ErrorKind::Deadline,
                    "execution deadline exceeded",
                ));
            }
            tokio::select! {permit=&mut acquire=>break permit.map_err(|_|Error::new(ErrorKind::Host,"worker pool closed"))?,_=tokio::time::sleep(Duration::from_millis(5))=>{}}
        };
        let job = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            script.call_with_keywords(&name, &args, &keywords, options)
        });
        let result = job
            .await
            .map_err(|e| Error::new(ErrorKind::Host, format!("script worker failed: {e}")))?;
        drop(guard);
        result
    }
}
struct CancelOnDrop(CancellationToken);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}
