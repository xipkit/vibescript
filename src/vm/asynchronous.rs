use super::{
    host_blocks::{BlockBoundary, HostControl, HostRequest, Started, abandoned},
    *,
};
use crate::{CallOptions, CancellationToken, Outcome, Script, budget::Charge};
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
    task::{Context, Poll, Wake, Waker},
    time::{Duration, Instant},
};
use tokio::{runtime::Handle, sync::Semaphore};

/// An async host method's scoped access to its invocation and attached block.
///
/// Values may outlive the callback; the handle and block cannot. A block runs
/// under the same limits, grants and control-flow rules as the receiving call.
/// Dropping a polled block future retires the invocation. An unpolled future has
/// no effect. The engine recovers any active worker before finishing a callback.
///
/// ```compile_fail
/// use vibescript::asynchronous::AsyncHostCall;
/// fn retain<'a>(call: &'a mut AsyncHostCall<'a>) -> &'static mut AsyncHostCall<'static> {
///     call
/// }
/// ```
///
/// Block calls require exclusive access to the invocation:
///
/// ```compile_fail
/// use vibescript::{HostMethod, Value};
/// HostMethod::new_async("visit", |call, _, _| Box::pin(async move {
///     let first = call.call_block(vec![]);
///     let second = call.call_block(vec![]);
///     let _ = tokio::join!(first, second);
///     Ok(Value::nil())
/// }));
/// ```
pub struct AsyncHostCall<'a> {
    backend: &'a mut dyn Backend,
}

trait Backend: Send {
    fn context(&mut self) -> Result<&mut CallContext>;
    fn block_given(&self) -> bool;
    fn receiver(&mut self) -> Result<Option<Value>>;
    fn set_receiver_field(&mut self, key: &[u8], value: &Value) -> Result<bool>;
    fn start_block(&mut self, args: Vec<Value>) -> Result<CancellationToken>;
    fn poll_block(&mut self, cx: &mut Context<'_>) -> Poll<Result<Value>>;
}

struct Invocation<'a, E: Holder, L: Lane<E>> {
    retirement: RetireOnDrop,
    execution: Option<E>,
    control: &'a mut HostControl,
    driver: Driver<L>,
    pending: Option<Driving<'a, E, L>>,
    failure: Option<L::Lost>,
    cancellation: CancellationToken,
}

impl AsyncHostCall<'_> {
    /// Returns this invocation's accounting and cancellation context.
    ///
    /// A dropped, previously polled block future may leave the context on a
    /// retiring worker. In that state access returns an error; no further script
    /// work can start. Previously latched quota errors keep their priority.
    pub fn context(&mut self) -> Result<&mut CallContext> {
        self.backend.context()
    }

    /// Reports whether the script attached a block to this method call.
    pub fn block_given(&self) -> bool {
        self.backend.block_given()
    }

    /// Returns an isolated, accounted snapshot of this call's member receiver.
    ///
    /// The receiver is the object or hash the script selected the method from,
    /// captured when the callee was resolved. It stays available across waits
    /// and after block calls; a method reached without a member lookup returns
    /// `None`. Reading follows the same retiring and latched-error rules as
    /// [`Self::context`]. See [`crate::HostCall::receiver`] for the value rules.
    pub fn receiver(&mut self) -> Result<Option<Value>> {
        self.backend.receiver()
    }

    /// Stores a field in this call's member receiver and publishes it to the script.
    ///
    /// Follows the same rules as [`crate::HostCall::set_receiver_field`], and
    /// the same retiring and latched-error rules as [`Self::context`].
    pub fn set_receiver_field(&mut self, key: &[u8], value: &Value) -> Result<bool> {
        self.backend.set_receiver_field(key, value)
    }

    /// Runs the attached block with owned arguments on the receiving runner.
    ///
    /// Each result is an isolated snapshot that later block calls cannot change.
    /// Arguments are isolated and accounted before script execution. `break`
    /// and nonlocal `return` remain pending even if the host ignores the returned
    /// control-flow error. Cancellation, exhausted limits and internal errors
    /// cannot be swallowed.
    pub async fn call_block(&mut self, args: Vec<Value>) -> Result<Value> {
        let cancellation = self.backend.start_block(args)?;
        let mut guard = RetireOnDrop(Some(cancellation));
        let result = std::future::poll_fn(|cx| self.backend.poll_block(cx)).await;
        guard.0 = None;
        result
    }
}

impl<'a, E: Holder + 'a, L: Lane<E>> Backend for Invocation<'a, E, L> {
    fn context(&mut self) -> Result<&mut CallContext> {
        if let Some(error) = &self.failure {
            return Err(L::error(error.clone()));
        }
        self.execution
            .as_mut()
            .map(|execution| &mut execution.owned().execution.context)
            .ok_or_else(retiring)
    }

    fn block_given(&self) -> bool {
        self.control.block.is_some()
    }

    fn receiver(&mut self) -> Result<Option<Value>> {
        if let Some(error) = &self.failure {
            return Err(L::error(error.clone()));
        }
        self.execution
            .as_mut()
            .ok_or_else(retiring)?
            .owned()
            .receiver(self.control)
    }

    fn set_receiver_field(&mut self, key: &[u8], value: &Value) -> Result<bool> {
        if let Some(error) = &self.failure {
            return Err(L::error(error.clone()));
        }
        self.execution
            .as_mut()
            .ok_or_else(retiring)?
            .owned()
            .set_receiver_field(self.control, key, value)
    }

    fn start_block(&mut self, args: Vec<Value>) -> Result<CancellationToken> {
        if let Some(error) = &self.failure {
            return Err(L::error(error.clone()));
        }
        let execution = self.execution.as_mut().ok_or_else(retiring)?;
        let block = self
            .control
            .block(&mut execution.owned().execution.context)?;
        let charge = execution
            .owned()
            .execution
            .context
            .reserve(size_of::<Work<E>>())?;
        let mut execution = self.execution.take().ok_or_else(retiring)?;
        execution.started();
        let work = Box::new(Work {
            execution,
            scope: Scope::PendingBlock,
            next: Next::StartBlock(block, args),
            charge,
        });
        match L::boxed(self.driver.clone(), work) {
            Ok(future) => self.pending = Some(future),
            Err((work, error)) => {
                let Completed { execution, result } = work.complete(Err(error));
                self.execution = Some(execution);
                return self
                    .control
                    .completed(result)
                    .map(|_| self.cancellation.clone());
            }
        }
        Ok(self.cancellation.clone())
    }

    fn poll_block(&mut self, cx: &mut Context<'_>) -> Poll<Result<Value>> {
        let Some(pending) = &mut self.pending else {
            let result = self.context().and_then(|context| {
                context.latch_internal(Err(violation("block must have pending work").into()))
            });
            return Poll::Ready(result);
        };
        let Poll::Ready(result) = Pin::new(pending).poll(cx) else {
            return Poll::Pending;
        };
        self.pending = None;
        Poll::Ready(match result {
            Ok(Completed { execution, result }) => {
                self.execution = Some(execution);
                self.control.completed(result)
            }
            Err(error) => {
                self.cancellation.cancel();
                self.failure = Some(error.clone());
                Err(L::error(error))
            }
        })
    }
}

impl<'a, E: Holder + 'a, L: Lane<E>> Invocation<'a, E, L> {
    async fn recover(&mut self) -> std::result::Result<(), L::Lost> {
        if self.pending.is_some() {
            let _result = std::future::poll_fn(|cx| self.poll_block(cx)).await;
        }
        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

struct RetireOnDrop(Option<CancellationToken>);
impl Drop for RetireOnDrop {
    fn drop(&mut self) {
        if let Some(token) = &self.0 {
            token.cancel();
        }
    }
}

fn retiring() -> Error {
    Error::new(ErrorKind::Cancelled, "block execution is retiring")
}

struct Owned {
    execution: Box<Execution>,
    charge: Option<Charge>,
    #[cfg(test)]
    on_finish: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl Owned {
    fn new(mut execution: Execution) -> Result<Self> {
        let charge = match execution.context.reserve(size_of::<Execution>()) {
            Ok(charge) => charge,
            Err(error) => {
                execution.finish(Err(error))?;
                return Err(Error::from(violation(
                    "failed reservation must terminate execution",
                )));
            }
        };
        Ok(Self {
            execution: Box::new(execution),
            charge,
            #[cfg(test)]
            on_finish: None,
        })
    }

    fn receiver(&mut self, control: &HostControl) -> Result<Option<Value>> {
        let execution = &mut *self.execution;
        execution.context.checkpoint()?;
        let result = (|| {
            let run = execution
                .run
                .as_ref()
                .ok_or(violation("execution must have a run"))?;
            control.receiver(&mut execution.context, &run.storage)
        })();
        execution.context.latch_internal(result)
    }

    fn set_receiver_field(
        &mut self,
        control: &mut HostControl,
        key: &[u8],
        value: &Value,
    ) -> Result<bool> {
        let execution = &mut *self.execution;
        execution.context.checkpoint()?;
        let result = (|| {
            let run = execution
                .run
                .as_mut()
                .ok_or(violation("execution must have a run"))?;
            control.set_receiver_field(&mut execution.context, &mut run.storage, key, value)
        })();
        execution.context.latch_internal(result)
    }

    fn finish(self, result: Result<Exit>) -> Result<Outcome> {
        #[cfg(test)]
        if let Some(on_finish) = &self.on_finish {
            on_finish();
        }
        let Self {
            execution, charge, ..
        } = self;
        let execution = *execution;
        drop(charge);
        execution.finish(result.and_then(|exit| match exit {
            Exit::Value(value) => Ok(value),
            Exit::Control(_) => Err(crate::checked::internal(
                "top-level call returned block control",
            )),
        }))
    }
}

trait Holder: Send {
    fn owned(&mut self) -> &mut Owned;
    fn started(&mut self) {}
    fn completed(&mut self) {}
}

impl Holder for Owned {
    fn owned(&mut self) -> &mut Owned {
        self
    }
}

// A host may catch a nested panic. Keep the unfinished block terminal even
// when its enclosing owner survives the unwind.
struct Lent<'a> {
    execution: &'a mut Owned,
    active: bool,
}

impl<'a> Lent<'a> {
    fn new(execution: &'a mut Owned) -> Self {
        Self {
            execution,
            active: false,
        }
    }
}

impl Holder for Lent<'_> {
    fn owned(&mut self) -> &mut Owned {
        self.execution
    }
    fn started(&mut self) {
        self.active = true;
    }
    fn completed(&mut self) {
        self.active = false;
    }
}

impl Drop for Lent<'_> {
    fn drop(&mut self) {
        if self.active {
            let _result = self
                .execution
                .execution
                .context
                .latch_internal::<()>(Err(abandoned().into()));
        }
    }
}

enum Scope {
    Call,
    PendingBlock,
    Block {
        boundary: BlockBoundary,
        args: Buffer<Value>,
    },
}

// Work already stores this state in accounted heap storage. Another box would
// allocate again for every completed host call.
#[allow(clippy::large_enum_variant)]
enum Next {
    Resume,
    StartBlock(Block, Vec<Value>),
    Host(HostRequest, HostControl, Result<Value>),
}

struct Work<E: Holder = Owned> {
    execution: E,
    scope: Scope,
    next: Next,
    charge: Option<Charge>,
}

impl<E: Holder> Work<E> {
    fn complete(self, result: Result<Exit>) -> Completed<E> {
        let Self {
            mut execution,
            scope,
            next,
            charge,
        } = self;
        drop(next);
        drop(charge);
        let result = match scope {
            Scope::Call | Scope::PendingBlock => result,
            Scope::Block { boundary, args } => {
                drop(args);
                let invocation = &mut *execution.owned().execution;
                match invocation.run.as_mut() {
                    Some(run) => run.finish_block(&mut invocation.context, boundary, result),
                    None => invocation
                        .context
                        .checkpoint()
                        .and(Err(violation("execution must have a run").into())),
                }
            }
        };
        let result = execution.owned().execution.context.latch_internal(result);
        execution.completed();
        Completed { execution, result }
    }

    fn interrupted(mut self, error: Error) -> Completed<E> {
        let error = self
            .execution
            .owned()
            .execution
            .context
            .checkpoint()
            .err()
            .unwrap_or(error);
        self.complete(Err(error))
    }
}

struct Completed<E: Holder = Owned> {
    execution: E,
    result: Result<Exit>,
}

// A completion can be much smaller than a host request; keep the request inline
// to avoid an extra allocation at every host boundary.
#[allow(clippy::large_enum_variant)]
enum Progress<C = Completed, E: Holder = Owned> {
    Host(Box<Work<E>>, HostRequest),
    Complete(C),
}

impl Progress {
    fn complete<C: Completion>(self) -> Progress<C> {
        match self {
            Self::Host(work, request) => Progress::Host(work, request),
            Self::Complete(completed) => Progress::Complete(C::complete(completed)),
        }
    }
}

// Blocks return their execution; top-level calls finalize it before the worker
// releases its permit.
trait Completion: Send + 'static {
    fn complete(completed: Completed) -> Self;
    fn interrupted(
        driver: &Driver<Worker>,
        completed: Completed,
    ) -> impl Future<Output = Self> + Send;
}

impl Completion for Completed {
    fn complete(completed: Self) -> Self {
        completed
    }

    async fn interrupted(_: &Driver<Worker>, completed: Self) -> Self {
        completed
    }
}

impl Completion for Result<Outcome> {
    fn complete(Completed { execution, result }: Completed) -> Self {
        execution.finish(result)
    }

    async fn interrupted(driver: &Driver<Worker>, mut completed: Completed) -> Self {
        match driver.slots.clone().try_acquire_owned() {
            Ok(permit) => {
                crate::asynchronous::blocking(move || {
                    let _permit = permit;
                    Self::complete(completed)
                })
                .await?
            }
            Err(_) => {
                // A cancelled queued call must not wait for an unrelated busy
                // worker. Retire its execution under a permit in the background.
                let error = completed
                    .execution
                    .execution
                    .context
                    .checkpoint()
                    .err()
                    .or_else(|| completed.result.as_ref().err().cloned())
                    .unwrap_or_else(retiring);
                let error = completed.execution.execution.diagnose(error);
                completed.result = Err(error.clone());
                let slots = driver.slots.clone();
                driver.handle.spawn(async move {
                    let permit = slots.acquire_owned().await.ok();
                    let _result = crate::asynchronous::blocking(move || {
                        let _permit = permit;
                        Self::complete(completed)
                    })
                    .await;
                });
                Err(error)
            }
        }
    }
}

// Worker submission owns its execution. Inline work can borrow one, but cannot lose it.
trait Lane<E: Holder>: Clone + Send + Sync + 'static {
    type Lost: Clone + Send;
    fn error(lost: Self::Lost) -> Error;
    fn boxed<'a>(driver: Driver<Self>, work: Box<Work<E>>) -> Prepared<Driving<'a, E, Self>, E>
    where
        E: 'a;
}

#[derive(Clone)]
struct Worker;
#[derive(Clone)]
struct Inline;

impl Lane<Owned> for Worker {
    type Lost = Error;
    fn error(lost: Error) -> Error {
        lost
    }
    fn boxed<'a>(driver: Driver<Self>, work: Box<Work>) -> Prepared<Driving<'a, Owned, Self>> {
        boxed_work(work, move |work| driver.drive::<Completed>(work))
    }
}

impl<E: Holder> Lane<E> for Inline {
    type Lost = std::convert::Infallible;
    fn error(lost: Self::Lost) -> Error {
        match lost {}
    }
    fn boxed<'a>(driver: Driver<Self>, work: Box<Work<E>>) -> Prepared<Driving<'a, E, Self>, E>
    where
        E: 'a,
    {
        boxed_work(work, move |work| driver.drive(work))
    }
}

#[derive(Clone)]
struct Driver<L> {
    slots: Arc<Semaphore>,
    handle: Handle,
    _lane: L,
}

type Prepared<T, E = Owned> = std::result::Result<T, (Box<Work<E>>, Error)>;

type Driving<'a, E, L> = ScopedFuture<'a, Completed<E>, <L as Lane<E>>::Lost>;

struct ScopedFuture<'a, T, E> {
    future: Pin<Box<dyn Future<Output = std::result::Result<T, E>> + Send + 'a>>,
    _charge: Option<Charge>,
}

impl<T, E> Future for ScopedFuture<'_, T, E> {
    type Output = std::result::Result<T, E>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(cx)
    }
}

fn boxed_work<'a, F, T, X, E: Holder>(
    mut work: Box<Work<E>>,
    create: impl FnOnce(Box<Work<E>>) -> F,
) -> Prepared<ScopedFuture<'a, T, X>, E>
where
    F: Future<Output = std::result::Result<T, X>> + Send + 'a,
{
    let charge = match work
        .execution
        .owned()
        .execution
        .context
        .reserve(size_of::<F>())
    {
        Ok(charge) => charge,
        Err(error) => return Err((work, error)),
    };
    Ok(ScopedFuture {
        future: Box::pin(create(work)),
        _charge: charge,
    })
}

async fn invoke<'a, E: Holder + 'a, L: Lane<E>>(
    call: &mut Invocation<'a, E, L>,
    request: &HostRequest,
) -> std::result::Result<Result<Value>, L::Lost> {
    let watch = match call.context() {
        Ok(context) => {
            if let Err(error) = context.checkpoint() {
                return Ok(Err(error));
            }
            Watch::new(context)
        }
        Err(error) => return Ok(Err(error)),
    };
    let result = {
        let mut handle = AsyncHostCall { backend: call };
        let future = request.method.invoke_async(
            &mut handle,
            &request.args.positional.data,
            &request.args.keywords.buffer.data,
        );
        tokio::pin!(future);
        tokio::select! {
            biased;
            _ = watch.wait() => Err(retiring()),
            result = &mut future => result,
        }
    };
    call.recover().await?;
    Ok(result)
}

impl Driver<Inline> {
    async fn drive<E: Holder>(
        self,
        mut work: Box<Work<E>>,
    ) -> std::result::Result<Completed<E>, std::convert::Infallible> {
        loop {
            match worker(work, self.clone()) {
                Progress::Complete(result) => return Ok(result),
                Progress::Host(next, request) => work = self.clone().host(next, request).await?,
            }
        }
    }

    async fn host<E: Holder>(
        self,
        mut next: Box<Work<E>>,
        request: HostRequest,
    ) -> std::result::Result<Box<Work<E>>, std::convert::Infallible> {
        let execution = next.execution.owned();
        let cancellation = execution.execution.context.cancellation().clone();
        let mut control = HostControl::new(&request);
        let mut call = Invocation {
            retirement: RetireOnDrop(Some(cancellation.clone())),
            cancellation,
            execution: Some(Lent::new(execution)),
            control: &mut control,
            driver: self,
            pending: None,
            failure: None,
        };
        let result = invoke(&mut call, &request).await?;
        call.retirement.0 = None;
        drop(call);
        next.next = Next::Host(request, control, result);
        Ok(next)
    }

    fn inline<E: Holder>(&self, mut work: Box<Work<E>>, parker: Arc<Parker>) -> Completed<E> {
        loop {
            match worker(work, self.clone()) {
                Progress::Complete(result) => return result,
                Progress::Host(next, request) => {
                    let driver = self.clone();
                    match boxed_work(next, move |next| driver.host(next, request)) {
                        Ok(future) => {
                            work = match wait_on_worker(&self.handle, future, parker.clone()) {
                                Ok(work) => work,
                                Err(impossible) => match impossible {},
                            }
                        }
                        Err((next, error)) => return next.complete(Err(error)),
                    }
                }
            }
        }
    }
}

impl Driver<Worker> {
    async fn drive<C: Completion>(self, mut work: Box<Work>) -> Result<C> {
        loop {
            match self.submit(work).await? {
                Progress::Complete(result) => return Ok(result),
                Progress::Host(next, request) => work = self.clone().host(next, request).await?,
            }
        }
    }

    async fn host(self, mut next: Box<Work>, request: HostRequest) -> Result<Box<Work>> {
        let cancellation = next.execution.execution.context.cancellation().clone();
        let mut control = HostControl::new(&request);
        let mut call = Invocation {
            retirement: RetireOnDrop(Some(cancellation.clone())),
            cancellation,
            execution: Some(next.execution),
            control: &mut control,
            driver: self,
            pending: None,
            failure: None,
        };
        let result = invoke(&mut call, &request).await?;
        next.execution = call
            .execution
            .take()
            .ok_or(violation("recovered worker must have an execution"))?;
        call.retirement.0 = None;
        drop(call);
        next.next = Next::Host(request, control, result);
        Ok(next)
    }

    async fn submit<C: Completion>(&self, mut work: Box<Work>) -> Result<Progress<C>> {
        let context = &mut work.execution.execution.context;
        if let Err(error) = context.checkpoint() {
            return Ok(Progress::Complete(
                C::interrupted(self, work.interrupted(error)).await,
            ));
        }
        let permit = match crate::asynchronous::acquire(
            self.slots.clone(),
            context.cancellation(),
            context.options.deadline,
        )
        .await
        {
            Ok(permit) => permit,
            Err(error) => {
                return Ok(Progress::Complete(
                    C::interrupted(self, work.interrupted(error)).await,
                ));
            }
        };
        let driver = Driver {
            slots: self.slots.clone(),
            handle: self.handle.clone(),
            _lane: Inline,
        };
        crate::asynchronous::blocking(move || {
            let _permit = permit;
            worker(work, driver).complete()
        })
        .await
    }
}

struct Watch {
    cancellation: CancellationToken,
    deadline: Option<Instant>,
    memory: Arc<crate::budget::Memory>,
}

impl Watch {
    fn new(ctx: &CallContext) -> Self {
        Self {
            cancellation: ctx.cancellation().clone(),
            deadline: ctx.options.deadline,
            memory: ctx.identity(),
        }
    }

    async fn wait(&self) {
        loop {
            if self.cancellation.is_cancelled()
                || self
                    .deadline
                    .is_some_and(|deadline| Instant::now() >= deadline)
                || self.memory.interrupted.load(Ordering::Acquire)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}

struct Synchronous<'a> {
    execution: &'a mut Owned,
    control: HostControl,
    driver: Driver<Inline>,
}

/// Wakes the synchronous worker thread that waits in [`wait_on_worker`]:
/// the thread that made the parker, which parks between polls.
struct Parker {
    thread: std::thread::Thread,
    /// Whether a wake has arrived since the wait last looked. The park
    /// token alone can't say: a host future that blocks the thread during
    /// its poll, on a channel receive for one, parks it too and takes a
    /// token meant for this wait.
    ready: AtomicBool,
    _charge: Option<Charge>,
}

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.ready.store(true, Ordering::Release);
        self.thread.unpark();
    }
}

fn wait_on_worker<T, E>(
    handle: &Handle,
    mut future: ScopedFuture<'_, T, E>,
    parker: Arc<Parker>,
) -> std::result::Result<T, E> {
    // Enter the runtime for timers and spawned host tasks without nesting its
    // executor. The synchronous stack already occupies a blocking worker.
    let _entered = handle.enter();
    let waker = Waker::from(parker.clone());
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(result) = Pin::new(&mut future).poll(&mut context) {
            return result;
        }
        // The flag remembers a wake from during the poll or before the
        // park; a park that returns without one, spuriously or on a token
        // another wait left, only looks again.
        while !parker.ready.swap(false, Ordering::Acquire) {
            std::thread::park();
        }
    }
}

impl crate::host_call::Backend for Synchronous<'_> {
    fn context(&mut self) -> &mut CallContext {
        &mut self.execution.execution.context
    }

    fn block_given(&self) -> bool {
        self.control.block.is_some()
    }

    fn receiver(&mut self) -> Result<Option<Value>> {
        self.execution.receiver(&self.control)
    }

    fn set_receiver_field(&mut self, key: &[u8], value: &Value) -> Result<bool> {
        self.execution
            .set_receiver_field(&mut self.control, key, value)
    }

    fn call_block(&mut self, args: &[Value]) -> Result<Value> {
        let result = self.block(args);
        self.execution.execution.context.latch_internal(result)
    }
}

impl Synchronous<'_> {
    fn block(&mut self, args: &[Value]) -> Result<Value> {
        let execution = &mut *self.execution.execution;
        let block = self.control.block(&mut execution.context)?;
        let run = execution
            .run
            .as_mut()
            .ok_or(violation("execution must have a run"))?;
        let mut started = Started::new(&mut execution.context, run.block_boundary());
        let arguments = match run.start_block(started.context(), block, args) {
            Ok(arguments) => arguments,
            Err(error) => return self.control.completed(started.finish(run, Err(error))),
        };
        let charge = match started
            .context()
            .reserve(size_of::<Parker>() + 2 * size_of::<usize>())
        {
            Ok(charge) => charge,
            Err(error) => {
                drop(arguments);
                return self.control.completed(started.finish(run, Err(error)));
            }
        };
        let parker = Arc::new(Parker {
            thread: std::thread::current(),
            ready: AtomicBool::new(false),
            _charge: charge,
        });
        let charge = match started.context().reserve(size_of::<Work<Lent<'_>>>()) {
            Ok(charge) => charge,
            Err(error) => {
                drop(arguments);
                return self.control.completed(started.finish(run, Err(error)));
            }
        };
        let boundary = started.lend();
        let mut lent = Lent::new(self.execution);
        lent.started();
        let work = Box::new(Work {
            execution: lent,
            scope: Scope::Block {
                boundary,
                args: arguments,
            },
            next: Next::Resume,
            charge,
        });
        // Inline bridges retain ownership, including on cancellation and quota failure.
        let Completed {
            execution: _,
            result,
        } = self.driver.inline(work, parker);
        self.control.completed(result)
    }
}

#[inline(never)]
fn segment<E: Holder>(mut work: Box<Work<E>>) -> Progress<Completed<E>, E> {
    let mut pending = None;
    match std::mem::replace(&mut work.next, Next::Resume) {
        Next::Resume => (),
        Next::StartBlock(block, args) => {
            let execution = &mut *work.execution.owned().execution;
            let Some(run) = execution.run.as_mut() else {
                return Progress::Complete(
                    work.interrupted(violation("execution must have a run").into()),
                );
            };
            let boundary = run.block_boundary();
            work.scope = Scope::Block {
                boundary,
                args: Buffer::empty(),
            };
            match run.start_block(&mut execution.context, block, &args) {
                Ok(args) => work.scope = Scope::Block { boundary, args },
                Err(error) => return Progress::Complete(work.complete(Err(error))),
            }
        }
        Next::Host(request, control, result) => {
            let execution = &mut *work.execution.owned().execution;
            let Some(run) = execution.run.as_mut() else {
                return Progress::Complete(
                    work.interrupted(violation("execution must have a run").into()),
                );
            };
            pending = Some(run.finish_host(&mut execution.context, request, control, result));
        }
    }
    loop {
        let boundary = match work.scope {
            Scope::Call => None,
            Scope::PendingBlock => {
                return Progress::Complete(work.complete(Err(crate::checked::internal(
                    "pending block was not prepared",
                ))));
            }
            Scope::Block { boundary, .. } => Some(boundary.floor),
        };
        let execution = &mut *work.execution.owned().execution;
        let Some(run) = execution.run.as_mut() else {
            return Progress::Complete(
                work.interrupted(violation("execution must have a run").into()),
            );
        };
        match run.resume(&mut execution.context, boundary, pending.take()) {
            Ok(Step::Complete(exit)) => return Progress::Complete(work.complete(Ok(exit))),
            Err(error) => return Progress::Complete(work.complete(Err(error))),
            Ok(Step::Host) => (),
        }
        let request = match run.prepare_host(&mut execution.context) {
            Ok(request) => request,
            Err(error) => {
                pending = Some(Err(error));
                continue;
            }
        };
        return Progress::Host(work, request);
    }
}

#[inline(never)]
fn worker<E: Holder>(mut work: Box<Work<E>>, driver: Driver<Inline>) -> Progress<Completed<E>, E> {
    loop {
        match segment(work) {
            Progress::Host(next, request) if !request.method.is_async() => {
                work = synchronous(next, request, driver.clone());
            }
            progress => return progress,
        }
    }
}

#[inline(never)]
fn synchronous<E: Holder>(
    mut work: Box<Work<E>>,
    request: HostRequest,
    driver: Driver<Inline>,
) -> Box<Work<E>> {
    let mut backend = Synchronous {
        execution: work.execution.owned(),
        control: HostControl::new(&request),
        driver,
    };
    let result = request.method.invoke(
        &mut crate::HostCall::new(&mut backend),
        &request.args.positional.data,
        &request.args.keywords.buffer.data,
    );
    work.next = Next::Host(request, backend.control, result);
    work
}

pub(crate) async fn call(
    slots: Arc<Semaphore>,
    script: Script,
    name: String,
    args: Vec<Value>,
    keywords: Vec<(String, Value)>,
    options: CallOptions,
) -> Result<Outcome> {
    let permit =
        crate::asynchronous::acquire(slots.clone(), &options.cancellation, options.deadline)
            .await?;
    let execution = crate::asynchronous::blocking(move || {
        let _permit = permit;
        let mut execution = Owned::new(Execution::new(&script, &name, &args, &keywords, options)?)?;
        let charge = match execution.execution.context.reserve(size_of::<Work>()) {
            Ok(charge) => charge,
            Err(error) => {
                execution.finish(Err(error))?;
                return Err(Error::from(violation(
                    "failed reservation must terminate execution",
                )));
            }
        };
        Ok(Box::new(Work {
            execution,
            scope: Scope::Call,
            next: Next::Resume,
            charge,
        }))
    })
    .await??;
    let driver = Driver {
        slots,
        handle: Handle::current(),
        _lane: Worker,
    };
    driver.drive::<Result<Outcome>>(execution).await?
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::Notify;

    fn work(script: &Script, on_finish: Arc<dyn Fn() + Send + Sync>) -> Box<Work> {
        let mut execution =
            Owned::new(Execution::new(script, "run", &[], &[], CallOptions::default()).unwrap())
                .unwrap();
        execution.on_finish = Some(on_finish);
        let charge = execution
            .execution
            .context
            .reserve(size_of::<Work>())
            .unwrap();
        Box::new(Work {
            execution,
            scope: Scope::Call,
            next: Next::Resume,
            charge,
        })
    }

    #[test]
    fn an_unprepared_pending_block_latches_its_internal_error() {
        let script = crate::Engine::new()
            .compile("def run -> int; 1; end")
            .unwrap();
        let mut pending = work(&script, Arc::new(|| {}));
        pending.scope = Scope::PendingBlock;
        let Progress::Complete(mut completed) = segment(pending) else {
            panic!("an unprepared block must fail before asking the host")
        };
        let error = completed.result.err().expect("pending block must fail");
        assert_eq!(error.kind, ErrorKind::Internal);
        assert!(error.message.ends_with("pending block was not prepared"));
        let later = completed
            .execution
            .execution
            .context
            .checkpoint()
            .unwrap_err();
        assert_eq!(later.message, error.message);
    }

    #[test]
    fn top_level_finalization_keeps_its_worker_and_leaves_the_executor_responsive() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            for end in ["7", "raise 'failed'", "cancelled"] {
                let source = format!("class Item;property child: Item?;end;def run -> int;items: array<Item> = [];100.times {{ items << Item.new }};{};end",
                    if end == "cancelled" { "7" } else { end });
                let script = crate::Engine::new().compile(&source).unwrap();
                let slots = Arc::new(Semaphore::new(1));
                let entered = Arc::new(Notify::new());
                let (release, released) = std::sync::mpsc::channel::<()>();
                let released = crate::actor::Actor::new(released);
                let (pool, ready) = (slots.clone(), entered.clone());
                let executor = std::thread::current().id();
                let work = work(&script, Arc::new(move || {
                    assert_ne!(std::thread::current().id(), executor, "finalization ran on the executor");
                    assert_eq!(pool.available_permits(), 0, "finalization lost its worker permit");
                    ready.notify_one();
                    assert!(released.with(|released| released.recv_timeout(Duration::from_secs(3))).is_ok(), "executor could not resume during finalization");
                }));
                if end == "cancelled" { work.execution.execution.context.cancellation().cancel(); }
                let driver = Driver { slots: slots.clone(), handle: Handle::current(), _lane: Worker };
                let mut call = Box::pin(driver.drive::<Result<Outcome>>(work));
                tokio::select! {
                    _ = &mut call => panic!("the call finished before its finalization began"),
                    entered = tokio::time::timeout(Duration::from_secs(3), entered.notified()) => {
                        entered.unwrap();
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
                assert_eq!(slots.available_permits(), 0);
                release.send(()).unwrap();
                let result = call.await.unwrap();
                match end {
                    "7" => {
                        let outcome = result.unwrap();
                        assert_eq!(outcome.value.as_int(), Some(7));
                        assert_eq!(outcome.stats.retained_memory_bytes, 0);
                    }
                    "cancelled" => assert_eq!(result.unwrap_err().kind, ErrorKind::Cancelled),
                    _ => assert_eq!(result.unwrap_err().kind, ErrorKind::Runtime),
                }
                assert_eq!(slots.available_permits(), 1);
            }
        });
        runtime.shutdown_timeout(Duration::from_secs(1));
    }

    #[test]
    fn cancelled_queued_calls_return_before_their_bounded_cleanup() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let script = crate::Engine::new()
                .compile("def run -> int;7;end")
                .unwrap();
            let slots = Arc::new(Semaphore::new(1));
            let busy = slots.clone().acquire_owned().await.unwrap();
            let finished = Arc::new(AtomicBool::new(false));
            let entered = Arc::new(Notify::new());
            let (pool, done, ready) = (slots.clone(), finished.clone(), entered.clone());
            let executor = std::thread::current().id();
            let work = work(
                &script,
                Arc::new(move || {
                    assert_ne!(std::thread::current().id(), executor);
                    assert_eq!(pool.available_permits(), 0);
                    done.store(true, Ordering::SeqCst);
                    ready.notify_one();
                }),
            );
            work.execution.execution.context.cancellation().cancel();
            let driver = Driver {
                slots: slots.clone(),
                handle: Handle::current(),
                _lane: Worker,
            };
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                driver.drive::<Result<Outcome>>(work),
            )
            .await
            .unwrap()
            .unwrap();
            let error = result.unwrap_err();
            assert_eq!(error.kind, ErrorKind::Cancelled);
            assert!(error.diagnostic.is_some());
            assert!(!finished.load(Ordering::SeqCst));
            assert_eq!(slots.available_permits(), 0);
            drop(busy);
            tokio::time::timeout(Duration::from_secs(3), entered.notified())
                .await
                .unwrap();
            // One blocking thread means this job runs after the cleanup job.
            crate::asynchronous::blocking(|| {}).await.unwrap();
            assert!(finished.load(Ordering::SeqCst));
            assert_eq!(slots.available_permits(), 1);
        });
        runtime.shutdown_timeout(Duration::from_secs(1));
    }
    #[tokio::test]
    async fn borrowed_nested_work_keeps_context_and_finalizes_once() {
        use std::{future::poll_fn, sync::atomic::AtomicUsize};
        for (body, mode) in [
            ("nested { leaf() }", "success"),
            ("nested { leaf() }", "abandon"),
            ("nested { leaf() }", "panic"),
            ("nested { leaf() }", "quota_panic"),
            ("leaf()", "success"),
            ("leaf()", "panic"),
            ("leaf()", "quota_panic"),
            ("sync_leaf()", "success"),
            ("sync_leaf()", "panic"),
            ("sync_leaf()", "quota_panic"),
        ] {
            let finished = Arc::new(AtomicUsize::new(0));
            let inspected = Arc::new(AtomicBool::new(false));
            let handlers = Arc::new(AtomicUsize::new(0));
            let mut engine = crate::Engine::new();
            let observed = inspected.clone();
            engine.register_method(
                "inspect",
                crate::HostMethod::new_with_block("inspect", move |call, _, _| {
                    let before = std::ptr::from_mut(call.context()).addr();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        call.call_block(&[])
                    }));
                    assert_eq!(std::ptr::from_mut(call.context()).addr(), before);
                    match mode {
                        "success" => {
                            assert_eq!(result.unwrap()?.as_int(), Some(7));
                            call.context().checkpoint()?;
                        }
                        "abandon" => {
                            assert_eq!(result.unwrap().unwrap_err().kind, ErrorKind::Cancelled);
                            assert_eq!(
                                call.context().checkpoint().unwrap_err().kind,
                                ErrorKind::Cancelled
                            );
                        }
                        _ => {
                            assert!(result.is_err(), "the host must observe the nested panic");
                            let expected = if mode == "quota_panic" {
                                ErrorKind::Memory
                            } else {
                                ErrorKind::Internal
                            };
                            assert_eq!(call.context().checkpoint().unwrap_err().kind, expected);
                            assert_eq!(call.call_block(&[]).unwrap_err().kind, expected);
                        }
                    }
                    observed.store(true, Ordering::SeqCst);
                    Ok(Value::int(999))
                }),
            );
            engine.register_method(
                "nested",
                crate::HostMethod::new_async("nested", move |call, _, _| {
                    Box::pin(async move {
                        let before = std::ptr::from_mut(call.context()?).addr();
                        if mode == "abandon" {
                            {
                                let future = call.call_block(vec![]);
                                tokio::pin!(future);
                                poll_fn(|cx| {
                                    assert!(future.as_mut().poll(cx).is_pending());
                                    Poll::Ready(())
                                })
                                .await;
                            }
                            assert_eq!(call.context().err().unwrap().kind, ErrorKind::Cancelled);
                            assert_eq!(
                                call.call_block(vec![]).await.unwrap_err().kind,
                                ErrorKind::Cancelled
                            );
                            Ok(Value::int(999))
                        } else {
                            let result = call.call_block(vec![]).await;
                            assert_eq!(std::ptr::from_mut(call.context()?).addr(), before);
                            result
                        }
                    })
                }),
            );
            engine.register_method(
                "leaf",
                crate::HostMethod::new_async("leaf", move |call, _, _| {
                    Box::pin(async move {
                        match mode {
                            "panic" => panic!("nested host panic"),
                            "quota_panic" => {
                                let _ignored = call.context()?.bytes(&vec![0; 65_536]);
                                panic!("nested host panic after quota");
                            }
                            "abandon" => std::future::pending::<()>().await,
                            _ => tokio::task::yield_now().await,
                        }
                        Ok(Value::int(7))
                    })
                }),
            );
            engine.register("sync_leaf", move |ctx, _| match mode {
                "panic" => panic!("synchronous host panic"),
                "quota_panic" => {
                    let _ignored = ctx.bytes(&vec![0; 65_536]);
                    panic!("synchronous host panic after quota");
                }
                _ => Ok(Value::int(7)),
            });
            let marked = handlers.clone();
            engine.register("mark", move |_, _| {
                marked.fetch_add(1, Ordering::SeqCst);
                Ok(Value::nil())
            });
            let script = engine.compile(&format!("def run -> any; begin; inspect {{ {body} }}; rescue; mark(); ensure; mark(); end; end")).unwrap();
            let done = finished.clone();
            let work = work(
                &script,
                Arc::new(move || {
                    done.fetch_add(1, Ordering::SeqCst);
                }),
            );
            // Set after construction so initialization is identical in every mode.
            let mut work = work;
            work.execution.execution.context.options.limits.memory_bytes = Some(32_768);
            let slots = Arc::new(Semaphore::new(1));
            let driver = Driver {
                slots: slots.clone(),
                handle: Handle::current(),
                _lane: Worker,
            };
            let result = tokio::time::timeout(
                Duration::from_secs(3),
                driver.drive::<Result<Outcome>>(work),
            )
            .await
            .unwrap()
            .unwrap();
            match mode {
                "success" => {
                    let result = result.unwrap();
                    assert_eq!(result.value.as_int(), Some(999));
                    assert_eq!(result.stats.retained_memory_bytes, 0);
                    assert_eq!(handlers.load(Ordering::SeqCst), 1);
                }
                _ => {
                    let expected = match mode {
                        "abandon" => ErrorKind::Cancelled,
                        "quota_panic" => ErrorKind::Memory,
                        _ => ErrorKind::Internal,
                    };
                    assert_eq!(result.unwrap_err().kind, expected, "{body}/{mode}");
                    assert_eq!(handlers.load(Ordering::SeqCst), 0, "{body}/{mode}");
                }
            }
            assert!(inspected.load(Ordering::SeqCst), "{body}/{mode}");
            assert_eq!(finished.load(Ordering::SeqCst), 1, "{body}/{mode}");
            assert_eq!(slots.available_permits(), 1, "{body}/{mode}");
        }
    }

    #[tokio::test]
    async fn missing_run_at_host_access_is_latched_before_the_host_can_ignore_it() {
        for prior in [false, true] {
            for lane in ["sync", "worker", "inline"] {
                for access in ["receiver", "set_receiver_field", "block"] {
                    let mut engine = crate::Engine::new();
                    engine.register_method(
                        "visit",
                        crate::HostMethod::new_with_block("visit", |_, _, _| Ok(Value::nil())),
                    );
                    let script = engine.compile("def run -> any; visit { 7 }; end").unwrap();
                    let Progress::Host(mut work, request) = segment(work(&script, Arc::new(|| {})))
                    else {
                        panic!("expected host request")
                    };
                    let run = work.execution.execution.run.take();
                    if prior {
                        work.execution.execution.context.options.limits.memory_bytes = Some(0);
                        assert_eq!(
                            work.execution
                                .execution
                                .context
                                .reserve(1)
                                .unwrap_err()
                                .kind,
                            ErrorKind::Memory
                        );
                    }
                    let expected = if prior {
                        ErrorKind::Memory
                    } else {
                        ErrorKind::Internal
                    };
                    let mut control = HostControl::new(&request);
                    let driver = Driver {
                        slots: Arc::new(Semaphore::new(1)),
                        handle: Handle::current(),
                        _lane: Inline,
                    };
                    let result = if lane == "sync" {
                        use crate::host_call::Backend as _;
                        let mut call = Synchronous {
                            execution: &mut work.execution,
                            control,
                            driver,
                        };
                        match access {
                            "receiver" => call.receiver().map(|_| ()),
                            "set_receiver_field" => {
                                call.set_receiver_field(b"x", &Value::int(1)).map(|_| ())
                            }
                            _ => call.call_block(&[]).map(|_| ()),
                        }
                    } else if lane == "inline" {
                        let cancellation = work.execution.execution.context.cancellation().clone();
                        let mut invocation = Invocation {
                            retirement: RetireOnDrop(None),
                            execution: Some(Lent::new(&mut work.execution)),
                            control: &mut control,
                            driver,
                            pending: None,
                            failure: None,
                            cancellation,
                        };
                        let mut call = AsyncHostCall {
                            backend: &mut invocation,
                        };
                        match access {
                            "receiver" => call.receiver().map(|_| ()),
                            "set_receiver_field" => {
                                call.set_receiver_field(b"x", &Value::int(1)).map(|_| ())
                            }
                            _ => call.call_block(vec![]).await.map(|_| ()),
                        }
                    } else {
                        let cancellation = work.execution.execution.context.cancellation().clone();
                        let mut invocation = Invocation {
                            retirement: RetireOnDrop(None),
                            execution: Some(work.execution),
                            control: &mut control,
                            driver: Driver {
                                slots: driver.slots,
                                handle: driver.handle,
                                _lane: Worker,
                            },
                            pending: None,
                            failure: None,
                            cancellation,
                        };
                        let mut call = AsyncHostCall {
                            backend: &mut invocation,
                        };
                        let result = match access {
                            "receiver" => call.receiver().map(|_| ()),
                            "set_receiver_field" => {
                                call.set_receiver_field(b"x", &Value::int(1)).map(|_| ())
                            }
                            _ => call.call_block(vec![]).await.map(|_| ()),
                        };
                        work.execution = invocation.execution.take().unwrap();
                        result
                    };
                    let error = result.unwrap_err();
                    assert_eq!(error.kind, expected, "{lane}/{access}/{prior}");
                    // Repair the injected state, then emulate a host ignoring the error.
                    work.execution.execution.run = run;
                    let later = work.execution.execution.context.checkpoint().unwrap_err();
                    assert_eq!(later.message, error.message, "{lane}/{access}");
                    let outcome = work.execution.finish(Ok(Exit::Value(Value::int(999))));
                    assert_eq!(outcome.unwrap_err().kind, expected, "{lane}/{access}");
                }
            }
        }
    }
}
