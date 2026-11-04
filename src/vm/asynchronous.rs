use super::{
    host_blocks::{BlockBoundary, HostControl, HostRequest},
    *,
};
use crate::{CallOptions, CancellationToken, Outcome, Script, budget::Charge};
use std::{
    future::Future,
    pin::Pin,
    sync::{Condvar, Mutex, atomic::Ordering},
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
    state: HostState<'a>,
}

enum HostState<'a> {
    Worker(&'a mut Invocation<Worker>),
    Inline(&'a mut Invocation<Inline>),
}

struct Invocation<L: Lane> {
    execution: Option<Owned>,
    control: Option<HostControl>,
    driver: Driver<L>,
    pending: Option<Driving<L>>,
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
        match &mut self.state {
            HostState::Worker(state) => state.context(),
            HostState::Inline(state) => state.context(),
        }
    }

    /// Reports whether the script attached a block to this method call.
    pub fn block_given(&self) -> bool {
        match &self.state {
            HostState::Worker(state) => state.block_given(),
            HostState::Inline(state) => state.block_given(),
        }
    }

    /// Returns an isolated, accounted snapshot of this call's member receiver.
    ///
    /// The receiver is the object or hash the script selected the method from,
    /// captured when the callee was resolved. It stays available across waits
    /// and after block calls; a method reached without a member lookup returns
    /// `None`. Reading follows the same retiring and latched-error rules as
    /// [`Self::context`]. See [`crate::HostCall::receiver`] for the value rules.
    pub fn receiver(&mut self) -> Result<Option<Value>> {
        match &mut self.state {
            HostState::Worker(state) => state.receiver(),
            HostState::Inline(state) => state.receiver(),
        }
    }

    /// Stores a field in this call's member receiver and publishes it to the script.
    ///
    /// Follows the same rules as [`crate::HostCall::set_receiver_field`], and
    /// the same retiring and latched-error rules as [`Self::context`].
    pub fn set_receiver_field(&mut self, key: &[u8], value: &Value) -> Result<bool> {
        match &mut self.state {
            HostState::Worker(state) => state.set_receiver_field(key, value),
            HostState::Inline(state) => state.set_receiver_field(key, value),
        }
    }

    /// Runs the attached block with owned arguments on the receiving runner.
    ///
    /// Each result is an isolated snapshot that later block calls cannot change.
    /// Arguments are isolated and accounted before script execution. `break`
    /// and nonlocal `return` remain pending even if the host ignores the returned
    /// control-flow error. Cancellation and exhausted limits cannot be swallowed.
    pub async fn call_block(&mut self, args: Vec<Value>) -> Result<Value> {
        match &mut self.state {
            HostState::Worker(state) => state.call_block(args).await,
            HostState::Inline(state) => state.call_block(args).await,
        }
    }
}

impl<L: Lane> Invocation<L> {
    fn context(&mut self) -> Result<&mut CallContext> {
        if let Some(error) = &self.failure {
            return Err(L::error(error.clone()));
        }
        self.execution
            .as_mut()
            .map(|execution| &mut execution.execution.context)
            .ok_or_else(retiring)
    }

    fn block_given(&self) -> bool {
        self.control.as_ref().unwrap().block.is_some()
    }

    fn receiver(&mut self) -> Result<Option<Value>> {
        if let Some(error) = &self.failure {
            return Err(L::error(error.clone()));
        }
        let execution = self.execution.as_mut().ok_or_else(retiring)?;
        let execution = &mut *execution.execution;
        self.control.as_ref().unwrap().receiver(
            &mut execution.context,
            &execution.run.as_ref().unwrap().storage,
        )
    }

    fn set_receiver_field(&mut self, key: &[u8], value: &Value) -> Result<bool> {
        if let Some(error) = &self.failure {
            return Err(L::error(error.clone()));
        }
        let execution = self.execution.as_mut().ok_or_else(retiring)?;
        let execution = &mut *execution.execution;
        self.control.as_mut().unwrap().set_receiver_field(
            &mut execution.context,
            &mut execution.run.as_mut().unwrap().storage,
            key,
            value,
        )
    }

    async fn call_block(&mut self, args: Vec<Value>) -> Result<Value> {
        if let Some(error) = &self.failure {
            return Err(L::error(error.clone()));
        }
        let execution = self.execution.as_mut().ok_or_else(retiring)?;
        let block = self
            .control
            .as_ref()
            .unwrap()
            .block(&mut execution.execution.context)?;
        let charge = execution.execution.context.reserve(size_of::<Work>())?;
        let mut work = Some(Box::new(Work {
            execution: self.execution.take().unwrap(),
            scope: Scope::PendingBlock,
            next: Next::StartBlock(block, args),
            charge,
        }));
        let future = match self.driver.clone().boxed(&mut work) {
            Ok(future) => future,
            Err(error) => {
                self.execution = Some(work.unwrap().execution);
                return Err(error);
            }
        };
        self.pending = Some(future);
        let mut guard = RetireOnDrop(Some(self.cancellation.clone()));
        let result = self.collect_block().await;
        guard.0 = None;
        result
    }

    async fn collect_block(&mut self) -> Result<Value> {
        let result = self.pending.as_mut().unwrap().await;
        self.pending = None;
        match result {
            Ok(Completed { execution, result }) => {
                self.execution = Some(execution);
                self.control.as_mut().unwrap().completed(result)
            }
            Err(error) => {
                self.cancellation.cancel();
                self.failure = Some(error.clone());
                Err(L::error(error))
            }
        }
    }

    async fn recover(&mut self) -> std::result::Result<(), L::Lost> {
        if self.pending.is_some() {
            let _result = self.collect_block().await;
        }
        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

impl<L: Lane> Drop for Invocation<L> {
    fn drop(&mut self) {
        if self.execution.is_some() || self.pending.is_some() {
            self.cancellation.cancel();
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
}

impl Owned {
    fn new(mut execution: Execution) -> Result<Self> {
        let charge = match execution.context.reserve(size_of::<Execution>()) {
            Ok(charge) => charge,
            Err(error) => return Err(execution.finish(Err(error)).unwrap_err()),
        };
        Ok(Self {
            execution: Box::new(execution),
            charge,
        })
    }

    fn finish(self, result: Result<Exit>) -> Result<Outcome> {
        let Self { execution, charge } = self;
        let execution = *execution;
        drop(charge);
        execution.finish(result.map(|exit| match exit {
            Exit::Value(value) => value,
            Exit::Control(_) => unreachable!(),
        }))
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

struct Work {
    execution: Owned,
    scope: Scope,
    next: Next,
    charge: Option<Charge>,
}

impl Work {
    fn complete(self: Box<Self>, result: Result<Exit>) -> Completed {
        let Self {
            mut execution,
            scope,
            next,
            charge,
        } = *self;
        drop(next);
        drop(charge);
        let result = match scope {
            Scope::Call | Scope::PendingBlock => result,
            Scope::Block { boundary, args } => {
                drop(args);
                let invocation = &mut *execution.execution;
                invocation.run.as_mut().unwrap().finish_block(
                    &mut invocation.context,
                    boundary,
                    result,
                )
            }
        };
        Completed { execution, result }
    }

    fn interrupted(mut self: Box<Self>, error: Error) -> Completed {
        let error = self
            .execution
            .execution
            .context
            .checkpoint()
            .err()
            .unwrap_or(error);
        self.complete(Err(error))
    }
}

struct Completed {
    execution: Owned,
    result: Result<Exit>,
}

enum Progress {
    Host(Box<Work>, HostRequest),
    Complete(Completed),
}

// Only worker submission can lose an execution. Inline calls keep ownership
// through host waits, cancellation and abandoned nested-block futures.
trait Lane: Clone + Send + Sync + 'static {
    type Lost: Clone + Send;

    fn error(lost: Self::Lost) -> Error;
    fn handle(state: &mut Invocation<Self>) -> AsyncHostCall<'_>;
    fn submit(
        driver: &Driver<Self>,
        work: Box<Work>,
    ) -> impl Future<Output = std::result::Result<Progress, Self::Lost>> + Send;
}

#[derive(Clone)]
struct Worker;
#[derive(Clone)]
struct Inline;

impl Lane for Worker {
    type Lost = Error;

    fn error(lost: Error) -> Error {
        lost
    }

    fn handle(state: &mut Invocation<Self>) -> AsyncHostCall<'_> {
        AsyncHostCall {
            state: HostState::Worker(state),
        }
    }

    async fn submit(driver: &Driver<Self>, work: Box<Work>) -> Result<Progress> {
        driver.submit(work).await
    }
}

impl Lane for Inline {
    type Lost = std::convert::Infallible;

    fn error(lost: Self::Lost) -> Error {
        match lost {}
    }

    fn handle(state: &mut Invocation<Self>) -> AsyncHostCall<'_> {
        AsyncHostCall {
            state: HostState::Inline(state),
        }
    }

    async fn submit(
        driver: &Driver<Self>,
        work: Box<Work>,
    ) -> std::result::Result<Progress, Self::Lost> {
        Ok(worker(work, driver.clone()))
    }
}

#[derive(Clone)]
struct Driver<L> {
    slots: Arc<Semaphore>,
    handle: Handle,
    _lane: L,
}

type Driving<L> = ScopedFuture<Completed, <L as Lane>::Lost>;

struct ScopedFuture<T, E> {
    future: Pin<Box<dyn Future<Output = std::result::Result<T, E>> + Send>>,
    _charge: Option<Charge>,
}

impl<T, E> Future for ScopedFuture<T, E> {
    type Output = std::result::Result<T, E>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.future.as_mut().poll(cx)
    }
}

fn boxed_work<F, T, E>(
    work: &mut Option<Box<Work>>,
    create: impl FnOnce(Box<Work>) -> F,
) -> Result<ScopedFuture<T, E>>
where
    F: Future<Output = std::result::Result<T, E>> + Send + 'static,
{
    let charge = work
        .as_mut()
        .unwrap()
        .execution
        .execution
        .context
        .reserve(size_of::<F>())?;
    Ok(ScopedFuture {
        future: Box::pin(create(work.take().unwrap())),
        _charge: charge,
    })
}

impl<L: Lane> Driver<L> {
    fn boxed(self, work: &mut Option<Box<Work>>) -> Result<Driving<L>> {
        boxed_work(work, move |work| self.drive(work))
    }

    async fn drive(self, mut work: Box<Work>) -> std::result::Result<Completed, L::Lost> {
        loop {
            let progress = L::submit(&self, work).await?;
            match progress {
                Progress::Complete(result) => return Ok(result),
                Progress::Host(next, request) => {
                    work = self.clone().host(next, request).await?;
                }
            }
        }
    }

    async fn host(
        self,
        mut next: Box<Work>,
        request: HostRequest,
    ) -> std::result::Result<Box<Work>, L::Lost> {
        let watch = Watch::new(&next.execution.execution.context);
        let checkpoint = next.execution.execution.context.checkpoint();
        let mut call = Invocation {
            cancellation: watch.cancellation.clone(),
            execution: Some(next.execution),
            control: Some(HostControl::new(&request)),
            driver: self,
            pending: None,
            failure: None,
        };
        let result = if let Err(error) = checkpoint {
            Err(error)
        } else {
            let mut handle = L::handle(&mut call);
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
        next.execution = call.execution.take().unwrap();
        next.next = Next::Host(request, call.control.take().unwrap(), result);
        Ok(next)
    }
}

impl Driver<Inline> {
    fn inline(&self, mut work: Box<Work>, parker: Arc<Parker>) -> Completed {
        loop {
            match worker(work, self.clone()) {
                Progress::Complete(result) => return result,
                Progress::Host(next, request) => {
                    let mut next = Some(next);
                    let driver = self.clone();
                    match boxed_work(&mut next, move |next| driver.host(next, request)) {
                        Ok(future) => {
                            work = match wait_on_worker(&self.handle, future, parker.clone()) {
                                Ok(work) => work,
                                Err(impossible) => match impossible {},
                            }
                        }
                        Err(error) => return next.unwrap().complete(Err(error)),
                    }
                }
            }
        }
    }
}

impl Driver<Worker> {
    async fn submit(&self, mut work: Box<Work>) -> Result<Progress> {
        let context = &mut work.execution.execution.context;
        if let Err(error) = context.checkpoint() {
            return Ok(Progress::Complete(work.interrupted(error)));
        }
        let permit = match crate::asynchronous::acquire(
            self.slots.clone(),
            context.cancellation(),
            context.options.deadline,
        )
        .await
        {
            Ok(permit) => permit,
            Err(error) => return Ok(Progress::Complete(work.interrupted(error))),
        };
        let driver = Driver {
            slots: self.slots.clone(),
            handle: self.handle.clone(),
            _lane: Inline,
        };
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            worker(work, driver)
        })
        .await
        .map_err(worker_error)
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

struct Synchronous {
    execution: Option<Owned>,
    control: HostControl,
    driver: Driver<Inline>,
}

struct Parker {
    ready: Mutex<bool>,
    changed: Condvar,
    _charge: Option<Charge>,
}

impl Wake for Parker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        *self.ready.lock().unwrap() = true;
        self.changed.notify_one();
    }
}

fn wait_on_worker<T, E>(
    handle: &Handle,
    mut future: ScopedFuture<T, E>,
    parker: Arc<Parker>,
) -> std::result::Result<T, E> {
    // Enter the runtime for timers and spawned host tasks without nesting its
    // executor. The synchronous stack already occupies a blocking worker.
    let _entered = handle.enter();
    let waker = Waker::from(parker.clone());
    let mut context = Context::from_waker(&waker);
    loop {
        *parker.ready.lock().unwrap() = false;
        if let Poll::Ready(result) = Pin::new(&mut future).poll(&mut context) {
            return result;
        }
        let mut ready = parker.ready.lock().unwrap();
        while !*ready {
            ready = parker.changed.wait(ready).unwrap();
        }
    }
}

impl crate::host_call::Backend for Synchronous {
    fn context(&mut self) -> &mut CallContext {
        &mut self.execution.as_mut().unwrap().execution.context
    }

    fn block_given(&self) -> bool {
        self.control.block.is_some()
    }

    fn receiver(&mut self) -> Result<Option<Value>> {
        let execution = &mut *self.execution.as_mut().unwrap().execution;
        self.control.receiver(
            &mut execution.context,
            &execution.run.as_ref().unwrap().storage,
        )
    }

    fn set_receiver_field(&mut self, key: &[u8], value: &Value) -> Result<bool> {
        let execution = &mut *self.execution.as_mut().unwrap().execution;
        self.control.set_receiver_field(
            &mut execution.context,
            &mut execution.run.as_mut().unwrap().storage,
            key,
            value,
        )
    }

    fn call_block(&mut self, args: &[Value]) -> Result<Value> {
        let execution = &mut *self.execution.as_mut().unwrap().execution;
        let block = self.control.block(&mut execution.context)?;
        let run = execution.run.as_mut().unwrap();
        let boundary = run.block_boundary();
        let arguments = match run.start_block(&mut execution.context, block, args) {
            Ok(arguments) => arguments,
            Err(error) => {
                let result = run.finish_block(&mut execution.context, boundary, Err(error));
                return self.control.completed(result);
            }
        };
        let charge = match execution
            .context
            .reserve(size_of::<Parker>() + 2 * size_of::<usize>())
        {
            Ok(charge) => charge,
            Err(error) => {
                drop(arguments);
                let result = run.finish_block(&mut execution.context, boundary, Err(error));
                return self.control.completed(result);
            }
        };
        let parker = Arc::new(Parker {
            ready: Mutex::new(false),
            changed: Condvar::new(),
            _charge: charge,
        });
        let charge = match execution.context.reserve(size_of::<Work>()) {
            Ok(charge) => charge,
            Err(error) => {
                drop(arguments);
                let result = run.finish_block(&mut execution.context, boundary, Err(error));
                return self.control.completed(result);
            }
        };
        let work = Box::new(Work {
            execution: self.execution.take().unwrap(),
            scope: Scope::Block {
                boundary,
                args: arguments,
            },
            next: Next::Resume,
            charge,
        });
        // Inline bridges retain ownership, including on cancellation and quota failure.
        let Completed { execution, result } = self.driver.inline(work, parker);
        self.execution = Some(execution);
        self.control.completed(result)
    }
}

#[inline(never)]
fn segment(mut work: Box<Work>) -> Progress {
    let mut pending = None;
    match std::mem::replace(&mut work.next, Next::Resume) {
        Next::Resume => (),
        Next::StartBlock(block, args) => {
            let execution = &mut *work.execution.execution;
            let run = execution.run.as_mut().unwrap();
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
            let execution = &mut *work.execution.execution;
            pending = Some(execution.run.as_mut().unwrap().finish_host(
                &mut execution.context,
                request,
                control,
                result,
            ));
        }
    }
    loop {
        let boundary = match work.scope {
            Scope::Call => None,
            Scope::PendingBlock => unreachable!(),
            Scope::Block { boundary, .. } => Some(boundary.floor),
        };
        let execution = &mut *work.execution.execution;
        match execution.run.as_mut().unwrap().resume(
            &mut execution.context,
            boundary,
            pending.take(),
        ) {
            Ok(Step::Complete(exit)) => return Progress::Complete(work.complete(Ok(exit))),
            Err(error) => return Progress::Complete(work.complete(Err(error))),
            Ok(Step::Host) => (),
        }
        let request = match execution
            .run
            .as_mut()
            .unwrap()
            .prepare_host(&mut execution.context)
        {
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
fn worker(mut work: Box<Work>, driver: Driver<Inline>) -> Progress {
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
fn synchronous(mut work: Box<Work>, request: HostRequest, driver: Driver<Inline>) -> Box<Work> {
    let mut backend = Synchronous {
        execution: Some(work.execution),
        control: HostControl::new(&request),
        driver,
    };
    let result = request.method.invoke(
        &mut crate::HostCall::new(&mut backend),
        &request.args.positional.data,
        &request.args.keywords.buffer.data,
    );
    work.execution = backend.execution.take().unwrap();
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
    let execution = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut execution = Owned::new(Execution::new(&script, &name, &args, &keywords, options)?)?;
        let charge = match execution.execution.context.reserve(size_of::<Work>()) {
            Ok(charge) => charge,
            Err(error) => return Err(execution.finish(Err(error)).unwrap_err()),
        };
        Ok(Box::new(Work {
            execution,
            scope: Scope::Call,
            next: Next::Resume,
            charge,
        }))
    })
    .await
    .map_err(worker_error)??;
    let driver = Driver {
        slots,
        handle: Handle::current(),
        _lane: Worker,
    };
    let Completed { execution, result } = driver.drive(execution).await?;
    execution.finish(result)
}

fn worker_error(error: tokio::task::JoinError) -> Error {
    if error.is_panic() {
        std::panic::resume_unwind(error.into_panic());
    }
    Error::new(ErrorKind::Host, format!("script worker failed: {error}"))
}
