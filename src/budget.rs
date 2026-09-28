use crate::{Error, ErrorKind, Result, Value};
use std::{
    collections::BTreeMap,
    mem::size_of,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};

pub(crate) const CHUNK: usize = 4096;

/// Names the configured step quota, as Go reports it.
pub(crate) fn step_quota_message(limit: u64) -> String {
    format!("step quota exceeded ({limit})")
}

/// Names the configured memory quota, as Go reports it.
pub(crate) fn memory_quota_message(limit: usize) -> String {
    format!("memory quota exceeded ({limit} bytes)")
}
pub(crate) const MAX_VALUE_DEPTH: usize = 10_000;
pub(crate) const MAX_ENVIRONMENT_DEPTH: usize = 128;

/// Execution limits. `None` disables the corresponding step or memory quota.
#[derive(Clone, Debug)]
pub struct Limits {
    pub steps: Option<u64>,
    pub memory_bytes: Option<usize>,
    pub recursion: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            steps: Some(1_000_000),
            memory_bytes: Some(16 << 20),
            recursion: 256,
        }
    }
}

/// A cheap, thread-safe signal observed at interpreter checkpoints.
#[derive(Clone, Default, Debug)]
pub struct CancellationToken(Arc<Signal>);

#[derive(Default, Debug)]
struct Signal {
    cancelled: AtomicBool,
    parent: Option<CancellationToken>,
}

impl CancellationToken {
    /// Creates an uncancelled token.
    pub fn new() -> Self {
        Self::default()
    }
    /// Requests cooperative cancellation of all calls sharing this token.
    pub fn cancel(&self) {
        self.0.cancelled.store(true, Ordering::Release);
    }
    /// Reports whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        let mut token = self;
        loop {
            if token.0.cancelled.load(Ordering::Acquire) {
                return true;
            }
            match &token.0.parent {
                Some(parent) => token = parent,
                None => return false,
            }
        }
    }
    /// Creates a child cancelled by its parent; cancelling the child leaves its parent unchanged.
    pub fn child_token(&self) -> Self {
        Self(Arc::new(Signal {
            cancelled: AtomicBool::new(false),
            parent: Some(self.clone()),
        }))
    }
}

/// Controls one invocation. Deadlines use a monotonic clock.
#[derive(Clone, Default, Debug)]
pub struct CallOptions {
    /// Per-call root bindings. Script mutations are isolated from these source values.
    ///
    /// Values are imported on first use, under this call's limits. Strict-effects
    /// scripts validate all globals as data-only before executing any script code.
    pub globals: BTreeMap<String, Value>,
    /// Explicit host grants, bound in order before script initialization.
    ///
    /// Capability bindings may contain host methods in strict-effects mode.
    /// Explicit [`Self::globals`] take precedence over same-named capability
    /// bindings. Factories run once for each call, including shadowed bindings.
    pub capabilities: Vec<crate::Capability>,
    pub limits: Limits,
    pub cancellation: CancellationToken,
    pub deadline: Option<Instant>,
    /// Permits `require` when the receiving script was compiled with strict effects.
    ///
    /// Defaults to false. This does not bypass module roots, allow/deny rules or limits.
    pub allow_require: bool,
}

/// Execution counters; memory measures tracked allocation capacity, not process RSS.
#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub steps: u64,
    pub peak_memory_bytes: usize,
    pub retained_memory_bytes: usize,
}

#[derive(Debug, Default)]
pub(crate) struct Memory {
    #[cfg(not(target_arch = "x86_64"))]
    used: AtomicUsize,
    #[cfg(target_arch = "x86_64")]
    reserved: AtomicUsize,
    #[cfg(target_arch = "x86_64")]
    released: AtomicUsize,
    peak: AtomicUsize,
    #[cfg(feature = "tokio")]
    pub(crate) interrupted: AtomicBool,
}

impl Memory {
    fn used(&self) -> usize {
        #[cfg(target_arch = "x86_64")]
        {
            // Lifetime totals may wrap, but checked live usage never does.
            self.reserved
                .load(Ordering::Relaxed)
                .wrapping_sub(self.released.load(Ordering::Relaxed))
        }
        #[cfg(not(target_arch = "x86_64"))]
        self.used.load(Ordering::Relaxed)
    }

    // On x86 only the executing call reserves, including its failure cleanup.
    // Releases can race on other threads. Their load is the reservation's
    // accounting point; publishing the total needs no read-modify-write.
    fn reserve(&self, bytes: usize) -> usize {
        #[cfg(target_arch = "x86_64")]
        {
            let reserved = self.reserved.load(Ordering::Relaxed);
            let used = reserved.wrapping_sub(self.released.load(Ordering::Relaxed));
            self.reserved
                .store(reserved.wrapping_add(bytes), Ordering::Relaxed);
            used + bytes
        }
        #[cfg(not(target_arch = "x86_64"))]
        {
            self.used.fetch_add(bytes, Ordering::Relaxed) + bytes
        }
    }

    // All peak publishers belong to the same executing call, including JSON's
    // exclusive reservation and failure cleanup. Remote drops only release.
    fn publish_peak(&self, peak: usize) {
        #[cfg(target_arch = "x86_64")]
        if peak > self.peak.load(Ordering::Relaxed) {
            self.peak.store(peak, Ordering::Relaxed);
        }
        // AArch64's ldumax is cheaper than the extra loads and branch here.
        #[cfg(not(target_arch = "x86_64"))]
        self.peak.fetch_max(peak, Ordering::Relaxed);
    }
}

#[derive(Debug)]
pub(crate) struct Charge {
    memory: Arc<Memory>,
    bytes: usize,
}

impl Charge {
    pub(crate) fn bytes(&self) -> usize {
        self.bytes
    }

    /// Live usage excluding a reservation's unused tail. Its owner settles
    /// the peak and releases that tail before any other reserve or return.
    pub(crate) fn reserved_usage(&self, unused: usize) -> usize {
        debug_assert!(unused <= self.bytes);
        self.memory.used() - unused
    }

    /// Publishes a peak accumulated by an exclusive, non-reentrant builder.
    pub(crate) fn publish_peak(&self, peak: usize) {
        self.memory.publish_peak(peak);
    }

    pub(crate) fn release(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        self.bytes -= bytes;
        #[cfg(target_arch = "x86_64")]
        self.memory.released.fetch_add(bytes, Ordering::Relaxed);
        #[cfg(not(target_arch = "x86_64"))]
        self.memory.used.fetch_sub(bytes, Ordering::Relaxed);
    }

    pub(crate) fn merge(into: &mut Option<Self>, other: Option<Self>) {
        if let Some(mut other) = other {
            if let Some(charge) = into {
                debug_assert!(Arc::ptr_eq(&charge.memory, &other.memory));
                charge.bytes += other.bytes;
                other.bytes = 0;
            } else {
                *into = Some(other);
            }
        }
    }
}

impl Drop for Charge {
    fn drop(&mut self) {
        #[cfg(target_arch = "x86_64")]
        self.memory
            .released
            .fetch_add(self.bytes, Ordering::Relaxed);
        #[cfg(not(target_arch = "x86_64"))]
        self.memory.used.fetch_sub(self.bytes, Ordering::Relaxed);
    }
}

/// A host callback's access to cooperative work and allocation accounting.
pub struct CallContext {
    pub(crate) options: CallOptions,
    pub(crate) strict_effects: bool,
    memory: Arc<Memory>,
    steps: u64,
    exhausted: Option<Error>,
    pub(crate) objects: Option<Arc<crate::objects::Heap>>,
    pub(crate) pending_objects:
        Buffer<(Arc<crate::objects::Instance>, Arc<crate::objects::Instance>)>,
    pub(crate) importing_objects: bool,
    pub(crate) snapshot_objects: Option<Buffer<(u64, Arc<crate::objects::Instance>)>>,
    pub(crate) snapshot_namespaces:
        Option<Buffer<(Arc<crate::code::Code>, Arc<crate::objects::Instance>)>>,
    pub(crate) namespace_depth: usize,
    pub(crate) scoped_sources: bool,
    pub(crate) has_exports: bool,
    pub(crate) code_roots: Option<Buffer<Arc<crate::code::Code>>>,
    pub(crate) host_roots: Option<Buffer<crate::capability::Root>>,
    pub(crate) capability_names: Buffer<Value>,
    pub(crate) enum_rebind: crate::enums::Rebind,
    pub(crate) random_source: Option<crate::random::Source>,
    pub(crate) output_writer: Option<crate::output::Writer>,
    pub(crate) error_writer: Option<crate::output::Writer>,
    pub(crate) random: Option<crate::random::Seeded>,
}

impl CallContext {
    pub(crate) fn new(options: CallOptions) -> Self {
        Self {
            options,
            strict_effects: false,
            memory: Arc::new(Memory::default()),
            steps: 0,
            exhausted: None,
            objects: None,
            pending_objects: Buffer::empty(),
            importing_objects: false,
            snapshot_objects: None,
            snapshot_namespaces: None,
            namespace_depth: 0,
            scoped_sources: false,
            has_exports: false,
            code_roots: None,
            host_roots: None,
            capability_names: Buffer::empty(),
            enum_rebind: crate::enums::Rebind::default(),
            random_source: None,
            output_writer: None,
            error_writer: None,
            random: None,
        }
    }

    /// Charges logical work. Exhaustion remains latched even if a callback ignores the error.
    #[inline]
    pub fn charge(&mut self, steps: u64) -> Result<()> {
        // Most charges stay within the quota and the current block of 16
        // steps, so no checkpoint is due and only the counter advances.
        let old = self.steps;
        if let Some(new) = old.checked_add(steps) {
            if old != 0
                && old / 16 == new / 16
                && self.exhausted.is_none()
                && self.options.limits.steps.is_none_or(|limit| new <= limit)
            {
                self.steps = new;
                return Ok(());
            }
        }
        self.charge_slowly(steps)
    }

    #[inline(never)]
    fn charge_slowly(&mut self, steps: u64) -> Result<()> {
        if let Some(err) = &self.exhausted {
            return Err(err.clone());
        }
        let old = self.steps;
        self.steps = match old.checked_add(steps) {
            Some(n) => n,
            None => return self.fail(ErrorKind::Steps, "step counter overflow"),
        };
        if let Some(limit) = self
            .options
            .limits
            .steps
            .filter(|&limit| self.steps > limit)
        {
            return self.fail(ErrorKind::Steps, step_quota_message(limit));
        }
        if old == 0 || old / 16 != self.steps / 16 {
            self.checkpoint()?;
        }
        Ok(())
    }

    /// Charges `units` single steps at once. It fails exactly where charging
    /// them one at a time would, leaving the same counter and latched error,
    /// so a loop can charge a run of elements before processing them.
    pub(crate) fn charge_each(&mut self, units: u64) -> Result<()> {
        if units == 0 {
            return Ok(());
        }
        if let Some(limit) = self.options.limits.steps {
            let room = limit.saturating_sub(self.steps);
            if units > room {
                return self.charge(room + 1);
            }
        }
        self.charge(units)
    }

    /// Charges and clears steps a loop deferred, such as the work counted by
    /// [`Buffer::extend_deferred`]. Nothing is charged when none are pending.
    pub(crate) fn charge_pending(&mut self, pending: &mut u64) -> Result<()> {
        match std::mem::take(pending) {
            0 => Ok(()),
            steps => self.charge(steps),
        }
    }

    /// Work that can be deferred without passing a quota or a periodic
    /// checkpoint. The borrower must settle it before using this context.
    #[inline]
    pub(crate) fn step_allowance(&self, maximum: u64) -> u64 {
        if self.steps == 0 || self.exhausted.is_some() {
            return 0;
        }
        maximum.min(15 - self.steps % 16).min(
            self.options
                .limits
                .steps
                .unwrap_or(u64::MAX)
                .saturating_sub(self.steps),
        )
    }

    /// Settles an allowance before its exclusive borrower uses the context
    /// again. The allowance excluded every quota and checkpoint boundary.
    #[inline]
    pub(crate) fn settle_step_allowance(&mut self, steps: u64) {
        debug_assert!(steps <= self.step_allowance(steps));
        self.steps += steps;
    }

    /// Fails with latched step exhaustion when `steps` further units cannot fit
    /// the quota, without consuming them. Materializers use it to reject work
    /// whose size is known up front before allocating or iterating.
    pub(crate) fn check_steps(&mut self, steps: u64) -> Result<()> {
        if let Some(err) = &self.exhausted {
            return Err(err.clone());
        }
        if let Some(limit) = self.options.limits.steps {
            if self
                .steps
                .checked_add(steps)
                .is_none_or(|total| total > limit)
            {
                return self.fail(ErrorKind::Steps, step_quota_message(limit));
            }
        }
        self.checkpoint()
    }

    /// Checks cancellation, deadline, and previously latched exhaustion immediately.
    #[inline]
    pub fn checkpoint(&mut self) -> Result<()> {
        if self.exhausted.is_none()
            && self.options.deadline.is_none()
            && !self.options.cancellation.is_cancelled()
        {
            return Ok(());
        }
        self.checkpoint_slowly()
    }

    #[inline(never)]
    fn checkpoint_slowly(&mut self) -> Result<()> {
        if let Some(err) = &self.exhausted {
            return Err(err.clone());
        }
        if self.options.cancellation.is_cancelled() {
            return self.fail(ErrorKind::Cancelled, "execution cancelled");
        }
        if self
            .options
            .deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return self.fail(ErrorKind::Deadline, "execution deadline exceeded");
        }
        Ok(())
    }

    /// What compilation charged to this context may still spend.
    pub(crate) fn budget(&self) -> crate::compilation::Budget {
        let exhausted = self.exhausted.is_some();
        crate::compilation::Budget {
            steps: self.options.limits.steps.map(|limit| {
                if exhausted {
                    0
                } else {
                    limit.saturating_sub(self.steps)
                }
            }),
            memory: self
                .options
                .limits
                .memory_bytes
                .map(|limit| limit.saturating_sub(self.memory.used())),
            deadline: self.options.deadline,
            cancellation: Some(self.options.cancellation.clone()),
        }
    }

    /// Returns the token for cooperative host operations.
    pub fn cancellation(&self) -> &CancellationToken {
        &self.options.cancellation
    }

    /// Returns the current counters, including allocations retained by returned values.
    pub fn stats(&self) -> Stats {
        Stats {
            steps: self.steps,
            peak_memory_bytes: self.memory.peak.load(Ordering::Relaxed),
            retained_memory_bytes: self.memory.used(),
        }
    }

    /// Copies bytes into a value charged to this execution.
    pub fn bytes(&mut self, bytes: &[u8]) -> Result<Value> {
        Value::copy_bytes(self, bytes, false)
    }

    /// Imports an array and all of its values into this execution's budget.
    pub fn array(&mut self, values: &[Value]) -> Result<Value> {
        let mut out = Buffer::with_capacity(self, values.len())?;
        for value in values {
            out.data.push(self.import(value)?);
        }
        Value::from_array(self, out)
    }

    pub(crate) fn work_bytes(&mut self, bytes: usize) -> Result<()> {
        self.charge((bytes as u64).div_ceil(64))?;
        self.checkpoint()
    }

    /// Records `units` of byte-sized scanning work, charging it like
    /// [`Self::work_bytes`] once a chunk has accumulated.
    ///
    /// A scan that examines one byte per loop iteration charges through
    /// `pending` rather than a step per byte; [`Self::settle_bytes`] charges
    /// the remainder when the scan stops.
    pub(crate) fn scan_bytes(&mut self, pending: &mut usize, units: usize) -> Result<()> {
        *pending += units;
        if *pending >= CHUNK {
            let units = std::mem::take(pending);
            self.work_bytes(units)?;
        }
        Ok(())
    }

    /// Charges scanning work left pending by [`Self::scan_bytes`].
    pub(crate) fn settle_bytes(&mut self, pending: &mut usize) -> Result<()> {
        let units = std::mem::take(pending);
        if units == 0 {
            return Ok(());
        }
        self.work_bytes(units)
    }

    pub(crate) fn fail<T>(&mut self, kind: ErrorKind, message: impl Into<String>) -> Result<T> {
        let err = Error::new(kind, message);
        if matches!(
            kind,
            ErrorKind::Steps
                | ErrorKind::OutputLimit
                | ErrorKind::Memory
                | ErrorKind::Recursion
                | ErrorKind::Cancelled
                | ErrorKind::Deadline
        ) && self.exhausted.is_none()
        {
            self.exhausted = Some(err.clone());
            #[cfg(feature = "tokio")]
            self.memory.interrupted.store(true, Ordering::Release);
        }
        Err(self.exhausted.clone().unwrap_or(err))
    }

    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted.is_some()
    }

    pub(crate) fn remember_exhaustion(&mut self, error: &Error) {
        if let Some(exhausted) = &mut self.exhausted {
            if exhausted.diagnostic.is_none() {
                exhausted.offset = error.offset;
                exhausted.diagnostic = error.diagnostic.clone();
            }
        }
    }

    pub(crate) fn guard<T>(&mut self, kind: ErrorKind, message: &str) -> Result<T> {
        self.checkpoint()?;
        Err(Error::limit(kind, message))
    }

    pub(crate) fn check_memory(&mut self, bytes: usize) -> Result<()> {
        self.checkpoint()?;
        let used = self.memory.used();
        let Some(next) = used.checked_add(bytes) else {
            return self.fail(ErrorKind::Memory, "memory size overflow");
        };
        if let Some(limit) = self
            .options
            .limits
            .memory_bytes
            .filter(|&limit| next > limit)
        {
            return self.fail(ErrorKind::Memory, memory_quota_message(limit));
        }
        Ok(())
    }

    pub(crate) fn reserve(&mut self, bytes: usize) -> Result<Option<Charge>> {
        self.check_memory(bytes)?;
        // Ownership controls import work even when the host sets no memory quota.
        // Execution allocates on one thread; returned values can be dropped on another.
        let actual = self.memory.reserve(bytes);
        self.memory.publish_peak(actual);
        Ok(Some(Charge {
            memory: self.memory.clone(),
            bytes,
        }))
    }

    /// Claims available headroom without failing or publishing a speculative
    /// peak. The caller checks interruption at each original consumption point
    /// and releases unused bytes before calling another allocator or returning.
    pub(crate) fn reserve_available(&mut self, maximum: usize) -> Charge {
        let used = self.memory.used();
        let limit = self.options.limits.memory_bytes.unwrap_or(usize::MAX);
        let bytes = maximum.min(limit.saturating_sub(used));
        self.memory.reserve(bytes);
        Charge {
            memory: self.memory.clone(),
            bytes,
        }
    }

    pub(crate) fn owns(&self, charge: &Option<Charge>) -> bool {
        charge
            .as_ref()
            .is_some_and(|charge| self.owns_charge(charge))
    }

    pub(crate) fn owns_charge(&self, charge: &Charge) -> bool {
        Arc::ptr_eq(&charge.memory, &self.memory)
    }

    pub(crate) fn identity(&self) -> Arc<Memory> {
        self.memory.clone()
    }
}

/// Elements larger than this, such as the VM's frames, iteration states and
/// pending argument lists, start a buffer at four rather than eight, since
/// those stacks rarely grow past four.
const LARGE: usize = 128;

#[derive(Debug)]
pub(crate) struct Buffer<T> {
    pub data: Vec<T>,
    charge: Option<Charge>,
}

impl<T> Buffer<T> {
    pub fn empty() -> Self {
        Self {
            data: Vec::new(),
            charge: None,
        }
    }
    pub fn untracked(data: Vec<T>) -> Self {
        Self { data, charge: None }
    }

    pub fn into_parts(self) -> (Vec<T>, Option<Charge>) {
        (self.data, self.charge)
    }

    pub fn with_capacity(ctx: &mut CallContext, capacity: usize) -> Result<Self> {
        let mut buf = Self::empty();
        buf.ensure(ctx, capacity)?;
        Ok(buf)
    }

    pub fn ensure(&mut self, ctx: &mut CallContext, capacity: usize) -> Result<()> {
        if capacity <= self.data.capacity() {
            return Ok(());
        }
        let bytes = capacity
            .checked_mul(size_of::<T>())
            .ok_or_else(|| Error::new(ErrorKind::Memory, "allocation size overflow"));
        let bytes = match bytes {
            Ok(n) => n,
            Err(_) => return ctx.fail(ErrorKind::Memory, "allocation size overflow"),
        };
        let mut charge = ctx.reserve(bytes)?;
        if self
            .data
            .try_reserve_exact(capacity - self.data.len())
            .is_err()
        {
            return ctx.fail(ErrorKind::Memory, "allocation failed");
        }
        if self.data.capacity() != capacity {
            charge = ctx.reserve(
                self.data
                    .capacity()
                    .checked_mul(size_of::<T>())
                    .ok_or_else(|| Error::new(ErrorKind::Memory, "allocation size overflow"))?,
            )?;
        }
        self.charge = charge;
        Ok(())
    }

    pub fn push(&mut self, ctx: &mut CallContext, value: T) -> Result<()> {
        if self.data.len() == self.data.capacity() {
            let capacity = if self.data.capacity() == 0 && size_of::<T>() > LARGE {
                4
            } else {
                self.data
                    .capacity()
                    .max(4)
                    .checked_mul(2)
                    .ok_or_else(|| Error::new(ErrorKind::Memory, "allocation size overflow"))?
            };
            self.ensure(ctx, capacity)?;
        }
        self.data.push(value);
        Ok(())
    }

    pub fn shrink(&mut self, ctx: &mut CallContext) -> Result<()> {
        if self.data.len() == self.data.capacity() {
            return Ok(());
        }
        if self.data.is_empty() {
            *self = Self::empty();
            return Ok(());
        }
        ctx.work_bytes(std::mem::size_of_val(self.data.as_slice()))?;
        let mut charge = ctx.reserve(std::mem::size_of_val(self.data.as_slice()))?;
        self.data.shrink_to_fit();
        if self.data.capacity() != self.data.len() {
            charge = ctx.reserve(self.data.capacity() * size_of::<T>())?;
        }
        self.charge = charge;
        ctx.checkpoint()
    }

    pub fn shrink_after_failure(&mut self, limit: Option<usize>) {
        if self.data.is_empty() {
            *self = Self::empty();
            return;
        }
        if self.data.len() > self.data.capacity() / 4 {
            return;
        }
        // Cleanup keeps allocation accounting active after work/cancellation has latched.
        // It cannot call user code or resume execution.
        let bytes = std::mem::size_of_val(self.data.as_slice());
        let temporary = if let Some(charge) = &self.charge {
            let used = charge.memory.used();
            let Some(next) = used.checked_add(bytes) else {
                return;
            };
            if limit.is_some_and(|limit| next > limit) {
                return;
            }
            let actual = charge.memory.reserve(bytes);
            charge.memory.publish_peak(actual);
            Some(Charge {
                memory: charge.memory.clone(),
                bytes,
            })
        } else {
            None
        };
        self.data.shrink_to_fit();
        if let Some(charge) = &mut self.charge {
            let bytes = self.data.capacity() * size_of::<T>();
            charge.release(charge.bytes - bytes);
        }
        drop(temporary);
    }
}

impl Buffer<u8> {
    /// Appends `bytes` exactly as [`Self::extend`] does, adding its work to
    /// `pending` instead of charging it. Pending steps are charged before the
    /// buffer grows, so growth and its failures follow the same charges.
    #[inline]
    pub fn extend_deferred(
        &mut self,
        ctx: &mut CallContext,
        bytes: &[u8],
        pending: &mut u64,
    ) -> Result<()> {
        let length = self.data.len().saturating_add(bytes.len());
        if length > self.data.capacity() {
            ctx.charge_pending(pending)?;
            self.ensure(ctx, length.max(self.data.capacity().saturating_mul(2)))?;
        }
        self.data.extend_from_slice(bytes);
        // One work charge per started 64 bytes, as `extend` charges each chunk.
        *pending += bytes
            .chunks(CHUNK)
            .map(|chunk| (chunk.len() as u64).div_ceil(64))
            .sum::<u64>();
        Ok(())
    }

    /// Appends `byte` exactly as [`Self::push`] does, which charges no work;
    /// pending steps are charged before the buffer grows.
    #[inline]
    pub fn push_deferred(
        &mut self,
        ctx: &mut CallContext,
        byte: u8,
        pending: &mut u64,
    ) -> Result<()> {
        if self.data.len() == self.data.capacity() {
            ctx.charge_pending(pending)?;
            let capacity = self
                .data
                .capacity()
                .max(4)
                .checked_mul(2)
                .ok_or_else(|| Error::new(ErrorKind::Memory, "allocation size overflow"))?;
            self.ensure(ctx, capacity)?;
        }
        self.data.push(byte);
        Ok(())
    }
}

impl<T: Clone> Buffer<T> {
    pub fn extend(&mut self, ctx: &mut CallContext, values: &[T]) -> Result<()> {
        let capacity = self
            .data
            .len()
            .checked_add(values.len())
            .ok_or_else(|| Error::new(ErrorKind::Memory, "allocation size overflow"))?;
        if capacity > self.data.capacity() {
            self.ensure(ctx, capacity.max(self.data.capacity().saturating_mul(2)))?;
        }
        for chunk in values.chunks(CHUNK / size_of::<T>().max(1)) {
            ctx.work_bytes(std::mem::size_of_val(chunk))?;
            self.data.extend_from_slice(chunk);
        }
        Ok(())
    }
}

#[cfg(test)]
mod accounting_tests {
    use super::*;
    use std::sync::Mutex;
    #[cfg(not(target_os = "wasi"))]
    use std::sync::mpsc;

    fn counters(ctx: &CallContext) -> (u64, usize, usize) {
        let stats = ctx.stats();
        (
            stats.steps,
            stats.peak_memory_bytes,
            stats.retained_memory_bytes,
        )
    }

    #[test]
    #[cfg(not(target_os = "wasi"))]
    fn remote_releases_restore_exact_headroom_and_preserve_exhaustion() {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: Some(100),
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let mut first = ctx.reserve(60).unwrap().unwrap();
        let (sent, received) = mpsc::channel();
        let (resume, resumed) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            first.release(20);
            sent.send(()).unwrap();
            resumed.recv().unwrap();
            drop(first);
        });
        received.recv().unwrap();
        assert_eq!(counters(&ctx), (0, 60, 40));
        let second = ctx.reserve(60).unwrap();
        assert_eq!(counters(&ctx), (0, 100, 100));
        let error = ctx.reserve(1).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory);
        assert_eq!(error.message, "memory quota exceeded (100 bytes)");
        resume.send(()).unwrap();
        worker.join().unwrap();
        drop(second);
        assert_eq!(counters(&ctx), (0, 100, 0));
        assert_eq!(ctx.reserve(0).unwrap_err(), error);
        assert_eq!(ctx.charge(1).unwrap_err(), error);
        assert_eq!(counters(&ctx), (0, 100, 0));
    }

    #[test]
    fn retained_result_releases_after_call_and_last_clone_on_another_thread() {
        let observed = Arc::new(Mutex::new(None));
        let saved = observed.clone();
        let mut engine = crate::Engine::new();
        engine.register("retain", move |ctx, _| {
            *saved.lock().unwrap() = Some(ctx.identity());
            ctx.bytes(b"retained result")
        });
        let result = engine
            .compile("def run -> any; retain; end")
            .unwrap()
            .call("run", &[], CallOptions::default())
            .unwrap();
        let memory = observed.lock().unwrap().take().unwrap();
        let retained = result.stats.retained_memory_bytes;
        assert!(retained > 0);
        assert_eq!(memory.used(), retained);
        let clone = result.value.clone();
        drop(result);
        assert_eq!(memory.used(), retained);
        #[cfg(not(target_os = "wasi"))]
        std::thread::spawn(move || drop(clone)).join().unwrap();
        #[cfg(target_os = "wasi")]
        drop(clone);
        assert_eq!(memory.used(), 0);
        let weak = Arc::downgrade(&memory);
        drop(memory);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn reservation_peak_and_cleanup_share_the_same_account() {
        let mut ctx = CallContext::new(CallOptions::default());
        let mut buffer = Buffer::<u8>::with_capacity(&mut ctx, 100).unwrap();
        buffer.data.extend_from_slice(b"0123456789");
        let mut available = ctx.reserve_available(1000);
        assert_eq!(available.reserved_usage(900), 200);
        available.publish_peak(200);
        available.release(900);
        assert_eq!(counters(&ctx), (0, 200, 200));
        let temporary = ctx.reserve(1).unwrap();
        assert_eq!(counters(&ctx), (0, 201, 201));
        drop(temporary);
        let error = ctx.charge(u64::MAX).unwrap_err();
        buffer.shrink_after_failure(None);
        assert_eq!(counters(&ctx), (u64::MAX, 210, 110));
        assert_eq!(ctx.checkpoint().unwrap_err(), error);
        drop(buffer);
        drop(available);
        assert_eq!(counters(&ctx), (u64::MAX, 210, 0));
    }

    #[test]
    fn cumulative_reservations_may_wrap_without_wrapping_live_usage() {
        let mut ctx = CallContext::new(CallOptions {
            limits: Limits {
                memory_bytes: None,
                ..Limits::default()
            },
            ..CallOptions::default()
        });
        let first = ctx.reserve(usize::MAX - 8).unwrap();
        assert_eq!(counters(&ctx), (0, usize::MAX - 8, usize::MAX - 8));
        drop(first);
        let second = ctx.reserve(16).unwrap();
        assert_eq!(counters(&ctx), (0, usize::MAX - 8, 16));
        drop(second);
        let full = ctx.reserve(usize::MAX).unwrap();
        assert_eq!(counters(&ctx), (0, usize::MAX, usize::MAX));
        assert_eq!(ctx.reserve(1).unwrap_err().message, "memory size overflow");
        drop(full);
        assert_eq!(counters(&ctx), (0, usize::MAX, 0));
    }

    #[test]
    #[cfg(not(target_os = "wasi"))]
    fn concurrent_drops_do_not_lose_reservations() {
        let mut ctx = CallContext::new(CallOptions::default());
        let (send, receive) = mpsc::sync_channel(32);
        let worker = std::thread::spawn(move || {
            while let Ok(charge) = receive.recv() {
                drop(charge);
            }
        });
        let retained = ctx.reserve(7).unwrap();
        for _ in 0..10_000 {
            send.send(ctx.reserve(13).unwrap()).unwrap();
            let temporary = ctx.reserve(11).unwrap();
            drop(temporary);
        }
        drop(send);
        worker.join().unwrap();
        assert_eq!(ctx.stats().retained_memory_bytes, 7);
        assert!((20..=7 + 34 * 13 + 11).contains(&ctx.stats().peak_memory_bytes));
        drop(retained);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn ignored_host_quota_errors_cannot_be_rescued_by_script() {
        for steps in [true, false] {
            let observed = Arc::new(Mutex::new(None));
            let saved = observed.clone();
            let mut engine = crate::Engine::new();
            engine.register("exhaust", move |ctx, _| {
                let error = if steps {
                    ctx.charge(1_000_000).unwrap_err()
                } else {
                    ctx.reserve(16 << 20).unwrap_err()
                };
                *saved.lock().unwrap() = Some((error, counters(ctx), ctx.identity()));
                Ok(Value::nil())
            });
            let error = engine
                .compile("def run -> int\n begin\n exhaust\n 1\n rescue\n 2\n end\nend")
                .unwrap()
                .call("run", &[], CallOptions::default())
                .unwrap_err();
            let (original, stats, memory) = observed.lock().unwrap().take().unwrap();
            assert_eq!(
                (error.kind, error.message),
                (original.kind, original.message)
            );
            assert_eq!(memory.peak.load(Ordering::Relaxed), stats.1);
            assert_eq!(memory.used(), 0);
        }
    }

    #[cfg(feature = "tokio")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn async_resumption_and_task_drops_keep_exact_accounting() {
        let observed = Arc::new(Mutex::new(None));
        let saved = observed.clone();
        let mut engine = crate::Engine::new();
        engine.register_method(
            "transfer",
            crate::HostMethod::new_async("transfer", move |call, _, _| {
                let saved = saved.clone();
                Box::pin(async move {
                    let (temporary, before, allocated) = {
                        let ctx = call.context()?;
                        *saved.lock().unwrap() = Some(ctx.identity());
                        let before = counters(ctx);
                        let value = ctx.bytes(&[b'x'; 512])?;
                        let allocated = counters(ctx);
                        (value, before, allocated)
                    };
                    tokio::spawn(async move { drop(temporary) }).await.unwrap();
                    tokio::task::yield_now().await;
                    let ctx = call.context()?;
                    assert_eq!(counters(ctx), (allocated.0, allocated.1, before.2));
                    ctx.bytes(b"async retained result")
                })
            }),
        );
        let script = engine.compile("def run -> any; transfer; end").unwrap();
        let result = crate::asynchronous::Runner::new(1)
            .unwrap()
            .call(script, "run".into(), vec![], CallOptions::default())
            .await
            .unwrap();
        let memory = observed.lock().unwrap().take().unwrap();
        assert_eq!(memory.used(), result.stats.retained_memory_bytes);
        assert!(result.stats.retained_memory_bytes > 0);
        tokio::task::spawn_blocking(move || drop(result))
            .await
            .unwrap();
        assert_eq!(memory.used(), 0);
    }
}

#[cfg(test)]
mod limit_tests {
    use super::*;
    use crate::{ErrorClass, json, regex};

    #[test]
    fn charging_a_run_matches_charging_each_step() {
        let context = |limit| {
            CallContext::new(CallOptions {
                limits: Limits {
                    steps: limit,
                    ..Limits::default()
                },
                ..CallOptions::default()
            })
        };
        for limit in [
            None,
            Some(0),
            Some(1),
            Some(15),
            Some(16),
            Some(17),
            Some(40),
        ] {
            for before in [0, 1, 15, 16, 39] {
                for units in [0, 1, 2, 16, 24, 41, 1000] {
                    let mut each = context(limit);
                    let mut run = context(limit);
                    let prepared = each.charge(before).is_ok();
                    assert_eq!(run.charge(before).is_ok(), prepared);
                    let expected = (0..units).try_for_each(|_| each.charge(1));
                    let actual = run.charge_each(units);
                    assert_eq!(
                        actual.map_err(|error| (error.kind, error.message)),
                        expected.map_err(|error| (error.kind, error.message)),
                        "{limit:?} {before} {units}"
                    );
                    assert_eq!(run.stats().steps, each.stats().steps);
                    assert_eq!(run.exhausted(), each.exhausted());
                }
            }
        }
    }

    #[test]
    fn rejected_json_and_regex_inputs_allow_further_work_without_retaining_scratch() {
        let mut ctx = CallContext::new(CallOptions::default());
        let oversized = vec![b'?'; (1 << 20) + 1];
        let before = ctx.stats();
        let error = json::parse_builtin(&mut ctx, &oversized, "JSON.parse").unwrap_err();
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(ctx.stats().peak_memory_bytes, before.peak_memory_bytes);
        assert_eq!(
            json::parse_builtin(&mut ctx, b"7", "JSON.parse")
                .unwrap()
                .as_int(),
            Some(7)
        );
        let deep = format!(
            "{}0{}",
            "[".repeat(MAX_VALUE_DEPTH + 1),
            "]".repeat(MAX_VALUE_DEPTH + 1)
        );
        let error = json::parse_builtin(&mut ctx, deep.as_bytes(), "JSON.parse").unwrap_err();
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        let error = regex::Utility::Match
            .call(
                &mut ctx,
                &[Value::bytes(b"a"), Value::bytes(oversized)],
                &[],
                false,
            )
            .unwrap_err();
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        let steps = ctx.stats().steps;
        ctx.charge(1).unwrap();
        assert_eq!(ctx.stats().steps, steps + 1);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
    }

    #[test]
    fn scan_table_guard_and_output_exhaustion_have_different_continuation_rules() {
        let mut ctx = CallContext::new(CallOptions::default());
        ctx.options.limits.steps = None;
        let error = regex::operations::member(
            &mut ctx,
            "scan",
            &Value::bytes(vec![b'a'; 20000]),
            &[Value::bytes(b"()".repeat(1000))],
            false,
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "string.scan match table exceeds limit 268435456 bytes"
        );
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        ctx.charge(1).unwrap();
        let error = regex::operations::member(
            &mut ctx,
            "scan",
            &Value::bytes(vec![b'a'; 40000]),
            &[Value::bytes(b"a")],
            false,
        )
        .unwrap_err();
        assert_eq!(
            error.message,
            "output limit exceeded: string.scan output exceeds limit 1048576 bytes"
        );
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        assert_eq!(ctx.checkpoint().unwrap_err(), error);
        assert_eq!(ctx.bytes(b"x").unwrap_err(), error);
    }

    #[test]
    fn guards_cannot_replace_existing_budget_exhaustion_or_host_cancellation() {
        for kind in [
            ErrorKind::Steps,
            ErrorKind::Memory,
            ErrorKind::Cancelled,
            ErrorKind::Deadline,
        ] {
            let mut ctx = CallContext::new(CallOptions::default());
            let error = match kind {
                ErrorKind::Steps => ctx.charge(u64::MAX).unwrap_err(),
                ErrorKind::Memory => ctx.check_memory(usize::MAX).unwrap_err(),
                ErrorKind::Cancelled => {
                    ctx.cancellation().cancel();
                    ctx.checkpoint().unwrap_err()
                }
                ErrorKind::Deadline => {
                    ctx.options.deadline = Some(Instant::now());
                    ctx.checkpoint().unwrap_err()
                }
                _ => unreachable!(),
            };
            let steps = ctx.stats().steps;
            assert_eq!(
                json::parse_builtin(&mut ctx, &vec![b'?'; (1 << 20) + 1], "JSON.parse")
                    .unwrap_err(),
                error
            );
            assert_eq!(ctx.bytes(b"x").unwrap_err(), error);
            assert_eq!(ctx.stats().steps, steps);
            assert_eq!(ctx.stats().retained_memory_bytes, 0);
        }
    }

    #[test]
    fn guards_observe_new_cancellation_and_deadlines_before_reporting_input_errors() {
        for deadline in [false, true] {
            let mut ctx = CallContext::new(CallOptions::default());
            if deadline {
                ctx.options.deadline = Some(Instant::now());
            } else {
                ctx.cancellation().cancel();
            }
            let error = json::parse_builtin(&mut ctx, &vec![b'?'; (1 << 20) + 1], "JSON.parse")
                .unwrap_err();
            assert_eq!(
                error.kind,
                if deadline {
                    ErrorKind::Deadline
                } else {
                    ErrorKind::Cancelled
                }
            );
            assert_eq!(error.class(), None);
            assert_eq!(ctx.checkpoint().unwrap_err(), error);
        }
    }

    #[test]
    fn foreign_entropy_errors_preserve_metadata_without_spending_the_current_budget() {
        let mut foreign = CallContext::new(CallOptions::default());
        let original = foreign.charge(u64::MAX).unwrap_err();
        let mut ctx = CallContext::new(CallOptions::default());
        let supplied = original.clone();
        ctx.random_source = Some(Arc::new(move |_, _| Err(supplied.clone())));
        let error = crate::random::Method::Id
            .call(&mut ctx, &[], &[], false)
            .unwrap_err();
        assert_eq!(error, original);
        assert_eq!(ctx.stats().retained_memory_bytes, 0);
        ctx.charge(1).unwrap();
        assert_eq!(
            json::parse_builtin(&mut ctx, b"7", "JSON.parse")
                .unwrap()
                .as_int(),
            Some(7)
        );
        assert_eq!(foreign.checkpoint().unwrap_err(), original);
    }
}
