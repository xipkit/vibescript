use crate::{CallContext, Result, Value};

pub(crate) trait Backend {
    fn context(&mut self) -> &mut CallContext;
    fn block_given(&self) -> bool;
    fn receiver(&mut self) -> Result<Option<Value>>;
    fn set_receiver_field(&mut self, key: &[u8], value: &Value) -> Result<bool>;
    fn call_block(&mut self, args: &[Value]) -> Result<Value>;
}

/// A synchronous host method's scoped access to its attached script block.
///
/// The handle borrows the active invocation. It cannot outlive the callback or
/// move to another thread. Values returned by a block may be retained; the block
/// itself cannot be stored, returned, or invoked after its receiving call ends.
///
/// A callback cannot extend the handle's lifetime:
///
/// ```compile_fail
/// use vibescript::HostCall;
/// fn retain<'a>(call: &'a mut HostCall<'a>) -> &'static mut HostCall<'static> {
///     call
/// }
/// ```
///
/// A block also cannot run on another thread, even within a scoped thread:
///
/// ```compile_fail
/// use vibescript::{HostMethod, Value};
/// HostMethod::new_with_block("visit", |call, _, _| {
///     std::thread::scope(|scope| {
///         scope.spawn(|| call.call_block(&[]));
///     });
///     Ok(Value::nil())
/// });
/// ```
pub struct HostCall<'a> {
    backend: &'a mut dyn Backend,
}

impl<'a> HostCall<'a> {
    pub(crate) fn new(backend: &'a mut dyn Backend) -> Self {
        Self { backend }
    }

    /// Returns the receiving invocation's accounting and cancellation context.
    pub fn context(&mut self) -> &mut CallContext {
        self.backend.context()
    }

    /// Reports whether the script attached a block to this method call.
    pub fn block_given(&self) -> bool {
        self.backend.block_given()
    }

    /// Returns an isolated, accounted snapshot of this call's member receiver.
    ///
    /// The receiver is the object or hash the script selected the method from,
    /// such as `cap` in `cap.send(1)`, `cap[:send](1)` or `cap::send(1)`.
    /// The script selects that receiver before evaluating arguments; later
    /// assignments to its binding do not replace the selected object. Once
    /// [`Self::set_receiver_field`] publishes, later reads follow the capability
    /// binding, including script writes made since. Each read
    /// snapshots its current data, including instances and module state.
    /// Later mutations do not change an earlier snapshot, which the host may keep.
    /// Using a snapshot does not replay module or class body initialization.
    /// A method reached without a member
    /// lookup, such as a registered global or a granted bare descriptor, has no
    /// receiver and returns `None`. Descriptors inside the snapshot keep this
    /// invocation's grant and cannot authorize a later call. Cancellation and
    /// latched quota errors surface here like any other boundary crossing.
    ///
    /// ```
    /// use vibescript::{CallOptions, Capability, Engine, HostMethod, Value};
    /// let read = HostMethod::new_with_block("counter.read", |call, _, _| {
    ///     let receiver = call.receiver()?.expect("member call");
    ///     let value = receiver
    ///         .as_hash()
    ///         .and_then(|entries| {
    ///             entries
    ///                 .iter()
    ///                 .find(|(key, _)| key.as_bytes() == Some(b"value".as_slice()))
    ///         })
    ///         .map(|(_, value)| value.clone());
    ///     Ok(value.unwrap_or_else(Value::nil))
    /// });
    /// let binding = move || {
    ///     Value::object(vec![
    ///         (b"value".to_vec(), Value::int(5)),
    ///         (b"read".to_vec(), read.value()),
    ///     ])
    /// };
    /// let mut engine = Engine::new();
    /// engine.declare_capability(&Capability::from_value("counter", binding()))?;
    /// let counter = Capability::new("counter", move |_| Ok(binding()));
    /// let script = engine.compile("counter.read()")?;
    /// let result = script.run(CallOptions {
    ///     capabilities: vec![counter],
    ///     ..CallOptions::default()
    /// })?;
    /// assert_eq!(result.value.as_int(), Some(5));
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn receiver(&mut self) -> Result<Option<Value>> {
        self.backend.receiver()
    }

    /// Stores `value` under `key` in this call's member receiver and publishes
    /// it to the script.
    ///
    /// A granted capability object, or a call global holding host methods, is
    /// the host's live state for the duration of one invocation. When the
    /// receiver is such an object, or a hash nested inside one, the write lands
    /// in that binding at once, as if the script had assigned `cap[key] = value`:
    /// later script reads, blocks this method runs and later host calls all
    /// observe it, and it ends with the invocation. Returns `true` in that case.
    /// The first publication finds the shallowest binding path holding the
    /// receiver; later publications in the same host call write to that path.
    ///
    /// Copies the script took earlier, such as `c = cap`, are independent
    /// values and do not change. A receiver no binding holds, such as a copy
    /// the script has since modified, only changes for this method's later
    /// [`Self::receiver`] reads, and the call returns `false`.
    ///
    /// The value is imported into this invocation's accounting and must be data
    /// or [`crate::HostMethod`] descriptors, as in a capability binding. A method
    /// reached without a member lookup has no receiver and returns an error.
    ///
    /// ```
    /// use vibescript::{CallOptions, Capability, Engine, HostMethod, Value};
    /// let install = HostMethod::new_with_block("config.install", |call, _, _| {
    ///     call.set_receiver_field(b"limit", &Value::int(10))?;
    ///     Ok(Value::nil())
    /// });
    /// let binding = move || {
    ///     Value::object(vec![
    ///         (b"limit".to_vec(), Value::int(0)),
    ///         (b"install".to_vec(), install.value()),
    ///     ])
    /// };
    /// let mut engine = Engine::new();
    /// engine.declare_capability(&Capability::from_value("config", binding()))?;
    /// let config = Capability::new("config", move |_| Ok(binding()));
    /// let script = engine.compile("config.install()\nconfig.limit + 1")?;
    /// let result = script.run(CallOptions {
    ///     capabilities: vec![config],
    ///     ..CallOptions::default()
    /// })?;
    /// assert_eq!(result.value.as_int(), Some(11));
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn set_receiver_field(&mut self, key: &[u8], value: &Value) -> Result<bool> {
        self.backend.set_receiver_field(key, value)
    }

    /// Runs the attached block synchronously with isolated, accounted arguments.
    /// Its returned value is also an isolated snapshot; later block calls cannot
    /// change a result retained by the host.
    ///
    /// Ordinary errors may be handled by the callback. A block's `break` or
    /// nonlocal `return` produces [`crate::ErrorKind::ControlFlow`]; the engine
    /// preserves that transfer even if the callback ignores it. Further block
    /// calls cannot run script code after such a transfer. Cancellation and
    /// exhausted step or memory quotas also remain latched.
    pub fn call_block(&mut self, args: &[Value]) -> Result<Value> {
        self.backend.context().checkpoint()?;
        self.backend.call_block(args)
    }
}
