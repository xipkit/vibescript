use crate::{CallContext, Error, ErrorKind, Result, Value};

type Block<'a> = dyn FnMut(&mut CallContext, &[Value]) -> Result<Value> + 'a;

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
    context: &'a mut CallContext,
    block: Option<&'a mut Block<'a>>,
}

impl<'a> HostCall<'a> {
    pub(crate) fn new(context: &'a mut CallContext, block: Option<&'a mut Block<'a>>) -> Self {
        Self { context, block }
    }

    /// Returns the receiving invocation's accounting and cancellation context.
    pub fn context(&mut self) -> &mut CallContext {
        self.context
    }

    /// Reports whether the script attached a block to this method call.
    pub fn block_given(&self) -> bool {
        self.block.is_some()
    }

    /// Runs the attached block synchronously with isolated, accounted arguments.
    ///
    /// Ordinary errors may be handled by the callback. A block's `break` or
    /// nonlocal `return` produces [`crate::ErrorKind::ControlFlow`]; the engine
    /// preserves that transfer even if the callback ignores it. Further block
    /// calls cannot run script code after such a transfer. Cancellation and
    /// exhausted step or memory quotas also remain latched.
    pub fn call_block(&mut self, args: &[Value]) -> Result<Value> {
        self.context.checkpoint()?;
        let block = self
            .block
            .as_mut()
            .ok_or_else(|| Error::new(ErrorKind::Argument, "block required"))?;
        block(self.context, args)
    }
}
