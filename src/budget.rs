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
pub(crate) const MAX_VALUE_DEPTH: usize = 128;

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
    used: AtomicUsize,
    peak: AtomicUsize,
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

    pub(crate) fn release(&mut self, bytes: usize) {
        assert!(bytes <= self.bytes);
        self.bytes -= bytes;
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
    pub fn charge(&mut self, steps: u64) -> Result<()> {
        if let Some(err) = &self.exhausted {
            return Err(err.clone());
        }
        let old = self.steps;
        self.steps = match old.checked_add(steps) {
            Some(n) => n,
            None => return self.fail(ErrorKind::Steps, "step counter overflow"),
        };
        if self
            .options
            .limits
            .steps
            .is_some_and(|limit| self.steps > limit)
        {
            return self.fail(ErrorKind::Steps, "step quota exceeded");
        }
        if old == 0 || old / 16 != self.steps / 16 {
            self.checkpoint()?;
        }
        Ok(())
    }

    /// Checks cancellation, deadline, and previously latched exhaustion immediately.
    pub fn checkpoint(&mut self) -> Result<()> {
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

    /// Returns the token for cooperative host operations.
    pub fn cancellation(&self) -> &CancellationToken {
        &self.options.cancellation
    }

    /// Returns the current counters, including allocations retained by returned values.
    pub fn stats(&self) -> Stats {
        Stats {
            steps: self.steps,
            peak_memory_bytes: self.memory.peak.load(Ordering::Relaxed),
            retained_memory_bytes: self.memory.used.load(Ordering::Relaxed),
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

    pub(crate) fn fail<T>(&mut self, kind: ErrorKind, message: &str) -> Result<T> {
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
        if self.options.limits.memory_bytes.is_none() {
            return Ok(());
        }
        let used = self.memory.used.load(Ordering::Relaxed);
        let Some(next) = used.checked_add(bytes) else {
            return self.fail(ErrorKind::Memory, "memory size overflow");
        };
        if self
            .options
            .limits
            .memory_bytes
            .is_some_and(|limit| next > limit)
        {
            return self.fail(ErrorKind::Memory, "memory quota exceeded");
        }
        Ok(())
    }

    pub(crate) fn reserve(&mut self, bytes: usize) -> Result<Option<Charge>> {
        self.check_memory(bytes)?;
        if self.options.limits.memory_bytes.is_none() {
            return Ok(None);
        }
        // Execution allocates on one thread; returned values can be dropped on another.
        let actual = self.memory.used.fetch_add(bytes, Ordering::Relaxed) + bytes;
        self.memory.peak.fetch_max(actual, Ordering::Relaxed);
        Ok(Some(Charge {
            memory: self.memory.clone(),
            bytes,
        }))
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
            let capacity = self
                .data
                .capacity()
                .max(4)
                .checked_mul(2)
                .ok_or_else(|| Error::new(ErrorKind::Memory, "allocation size overflow"))?;
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
            let used = charge.memory.used.load(Ordering::Relaxed);
            let Some(next) = used.checked_add(bytes) else {
                return;
            };
            if limit.is_some_and(|limit| next > limit) {
                return;
            }
            let actual = charge.memory.used.fetch_add(bytes, Ordering::Relaxed) + bytes;
            charge.memory.peak.fetch_max(actual, Ordering::Relaxed);
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
            charge
                .memory
                .used
                .fetch_sub(charge.bytes - bytes, Ordering::Relaxed);
            charge.bytes = bytes;
        }
        drop(temporary);
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
mod limit_tests {
    use super::*;
    use crate::{ErrorClass, json, regex};

    #[test]
    fn rejected_json_and_regex_inputs_allow_further_work_without_retaining_scratch() {
        let mut ctx = CallContext::new(CallOptions::default());
        let oversized = vec![b'?'; (1 << 20) + 1];
        let before = ctx.stats();
        let error = json::parse_builtin(&mut ctx, &oversized).unwrap_err();
        assert_eq!(error.class(), Some(ErrorClass::Limit));
        assert_eq!(ctx.stats().peak_memory_bytes, before.peak_memory_bytes);
        assert_eq!(
            json::parse_builtin(&mut ctx, b"7").unwrap().as_int(),
            Some(7)
        );
        let deep = format!("{}0{}", "[".repeat(129), "]".repeat(129));
        let error = json::parse_builtin(&mut ctx, deep.as_bytes()).unwrap_err();
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
        assert_eq!(error.message, "string.scan match table exceeds 256 MiB");
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
        assert_eq!(error.message, "string.scan output exceeds 1 MiB");
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
                json::parse_builtin(&mut ctx, &vec![b'?'; (1 << 20) + 1]).unwrap_err(),
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
            let error = json::parse_builtin(&mut ctx, &vec![b'?'; (1 << 20) + 1]).unwrap_err();
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
            json::parse_builtin(&mut ctx, b"7").unwrap().as_int(),
            Some(7)
        );
        assert_eq!(foreign.checkpoint().unwrap_err(), original);
    }
}
