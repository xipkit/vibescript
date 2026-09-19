use crate::{CallContext, Result, Value};

pub(crate) trait Backend {
    fn context(&mut self) -> &mut CallContext;
    fn block_given(&self) -> bool;
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
