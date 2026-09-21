use crate::{CallContext, Result, Value};

pub(crate) trait Backend {
    fn context(&mut self) -> &mut CallContext;
    fn block_given(&self) -> bool;
    fn receiver(&mut self) -> Result<Option<Value>>;
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
    /// assignments to its binding do not replace the selected object. Each read
    /// snapshots its current data, including instances and captured module state.
    /// Later mutations do not change an earlier snapshot, which the host may keep.
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
    /// let counter = Capability::new("counter", move |_| {
    ///     Ok(Value::object(vec![
    ///         (b"value".to_vec(), Value::int(5)),
    ///         (b"read".to_vec(), read.value()),
    ///     ]))
    /// });
    /// let script = Engine::new().compile("counter.read()")?;
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

    /// Runs the attached block synchronously with isolated, accounted arguments.
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
