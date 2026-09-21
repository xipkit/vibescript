use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Charge, value::Kind};
use std::{
    fmt,
    sync::{Arc, Weak},
};

type Binder = Arc<dyn Fn(&mut CallContext) -> Result<Value> + Send + Sync>;
type ArgumentContract =
    Arc<dyn Fn(&mut CallContext, &[Value], &[(Value, Value)], bool) -> Result<()> + Send + Sync>;
type ReturnContract = Arc<dyn Fn(&mut CallContext, &Value) -> Result<()> + Send + Sync>;
type BlockCallback = Arc<
    dyn Fn(&mut crate::HostCall<'_>, &[Value], &[(Value, Value)]) -> Result<Value> + Send + Sync,
>;
#[cfg(feature = "tokio")]
type AsyncCallback = Arc<
    dyn for<'a> Fn(
            &'a mut crate::asynchronous::AsyncHostCall,
            &'a [Value],
            &'a [(Value, Value)],
        ) -> crate::asynchronous::HostFuture<'a>
        + Send
        + Sync,
>;

#[derive(Clone)]
enum Callback {
    Plain(crate::HostCallback),
    Block(BlockCallback),
    #[cfg(feature = "tokio")]
    Async(AsyncCallback),
}

#[derive(Clone)]
pub(crate) enum Registered {
    Callback(crate::HostCallback),
    Method(HostMethod),
}

/// A host namespace granted explicitly to one script invocation.
///
/// A factory runs once before script initialization, under the receiving call's
/// context. Return an object containing data and [`HostMethod`] values, or a
/// method descriptor for a global callable. Cloning a capability shares its
/// factory; mutable per-call state belongs inside the factory. An immutable
/// binding built once on the host side can be granted through
/// [`Self::from_value`] instead, which also lets the static checker read it.
///
/// ```
/// use vibescript::{CallOptions, Capability, Engine, HostMethod, Value};
/// let sms = Capability::new("SMS", |_| {
///     let send = HostMethod::new("SMS.send", |ctx, _, _| {
///         ctx.charge(1)?;
///         ctx.bytes(b"queued")
///     });
///     Ok(Value::object(vec![(b"send".to_vec(), send.value())]))
/// });
/// let script = Engine::new().compile("SMS.send(\"hello\")")?;
/// let result = script.run(CallOptions {
///     capabilities: vec![sms],
///     ..CallOptions::default()
/// })?;
/// assert_eq!(result.value.as_bytes(), Some(b"queued".as_slice()));
/// # Ok::<(), vibescript::Error>(())
/// ```
#[derive(Clone)]
pub struct Capability {
    pub(crate) name: String,
    binding: Binding,
}

#[derive(Clone)]
enum Binding {
    Factory(Binder),
    Value(Value),
}

impl Capability {
    /// Creates a named per-call binding factory. Later grants replace earlier names.
    ///
    /// The factory is opaque to static checking: reports for calls granted a
    /// factory remain incomplete, because inspecting its binding would require
    /// running host code. Use it when each invocation needs fresh callback state.
    pub fn new(
        name: impl Into<String>,
        bind: impl Fn(&mut CallContext) -> Result<Value> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            binding: Binding::Factory(Arc::new(bind)),
        }
    }

    /// Creates a named grant from an immutable host binding template.
    ///
    /// The template is usually an object containing [`HostMethod`] descriptors.
    /// Every invocation imports this same value, so its data and published
    /// signatures can be checked statically without executing any host code,
    /// while each import still gives the methods that call's own fresh grant.
    /// Callbacks that need per-call state belong in a [`Self::new`] factory. A
    /// descriptor that was already returned from an earlier invocation keeps its
    /// expired grant and cannot authorize another call through this template.
    ///
    /// ```
    /// use vibescript::{CallOptions, Capability, Engine, HostMethod, Signature, SignatureParam, Value};
    /// let send = HostMethod::new("SMS.send", |ctx, _, _| ctx.bytes(b"queued"))
    ///     .with_signature(Signature {
    ///         params: vec![SignatureParam { name: "message".into(), ty: "string".into(), optional: false }],
    ///         result: "string".into(),
    ///         accepts_block: false,
    ///     })?;
    /// let sms = Capability::from_value("SMS", Value::object(vec![(b"send".to_vec(), send.value())]));
    /// let options = CallOptions { capabilities: vec![sms], ..CallOptions::default() };
    /// let script = Engine::new().compile("def run -> string; SMS.send(\"hello\"); end")?;
    /// assert!(script.check_call("run", &[], &options)?.is_clean());
    /// assert!(!Engine::new().compile("def run; SMS.send(1); end")?.check_call("run", &[], &options)?.is_clean());
    /// assert_eq!(script.call("run", &[], options)?.value.as_bytes(), Some(b"queued".as_slice()));
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn from_value(name: impl Into<String>, value: Value) -> Self {
        Self {
            name: name.into(),
            binding: Binding::Value(value),
        }
    }

    /// Exposes an immutable binding template to the checker; factories stay opaque.
    pub(crate) fn template(&self) -> Option<&Value> {
        match &self.binding {
            Binding::Factory(_) => None,
            Binding::Value(value) => Some(value),
        }
    }

    pub(crate) fn bind(&self, ctx: &mut CallContext) -> Result<Value> {
        ctx.checkpoint()?;
        let value = match &self.binding {
            Binding::Factory(bind) => bind(ctx),
            Binding::Value(value) => Ok(value.clone()),
        };
        ctx.checkpoint()?;
        ctx.import(&value?)
    }
}

impl fmt::Debug for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Capability")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

/// A host method with optional argument and return validation.
///
/// Methods accept positional and keyword arguments. They cannot be detached into
/// script values. Block-capable methods use [`Self::new_with_block`]. Callbacks and validators
/// must cooperate with cancellation and account their work through the context.
/// Arguments and results cross the host boundary as isolated snapshots, including
/// mutable instance and module state. Aliases within one argument list are preserved;
/// later script mutations do not change values retained by a callback or validator.
#[derive(Clone)]
pub struct HostMethod {
    definition: Arc<Definition>,
}

impl HostMethod {
    /// Creates a method. The qualified name appears in boundary diagnostics.
    pub fn new(
        name: impl Into<String>,
        callback: impl Fn(&mut CallContext, &[Value], &[(Value, Value)]) -> Result<Value>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            definition: Arc::new(Definition {
                name: name.into(),
                callback: Callback::Plain(Arc::new(callback)),
                arguments: None,
                result: None,
                signature: None,
            }),
        }
    }

    /// Creates a method that may synchronously invoke an attached script block.
    ///
    /// The scoped [`crate::HostCall`] supplies the original invocation's context
    /// and enforces the block's lifetime. The callback also runs when no block is
    /// attached; use its `block_given` method or a block contract to require one.
    /// The same handle exposes the member receiver the script called the method
    /// on through [`crate::HostCall::receiver`], so a descriptor installed into
    /// several objects can read the fields of whichever object was used. The
    /// receiver belongs to the invocation, never to this descriptor.
    pub fn new_with_block(
        name: impl Into<String>,
        callback: impl Fn(&mut crate::HostCall<'_>, &[Value], &[(Value, Value)]) -> Result<Value>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            definition: Arc::new(Definition {
                name: name.into(),
                callback: Callback::Block(Arc::new(callback)),
                arguments: None,
                result: None,
                signature: None,
            }),
        }
    }

    /// Creates an async method hosted by [`crate::asynchronous::Runner`].
    ///
    /// The future may borrow its scoped handle and arguments across waits. Use
    /// the handle's `call_block` to run an attached block on a bounded worker,
    /// and its `receiver` to snapshot the object the method was called on.
    /// Script-owned values and block storage remain accounted while suspended.
    /// The host must account its own work and keep each future poll bounded.
    /// A synchronous script call reports a catchable host error at this method.
    ///
    /// ```
    /// use vibescript::{CallOptions, Engine, HostMethod, asynchronous::Runner};
    /// let mut engine = Engine::new();
    /// engine.register_method("visit", HostMethod::new_async("visit", |call, args, _| {
    ///     Box::pin(async move {
    ///         tokio::task::yield_now().await;
    ///         call.context()?.charge(1)?;
    ///         call.call_block(args.to_vec()).await
    ///     })
    /// }));
    /// let script = engine.compile("def run;visit(20){|n|n+1};end")?;
    /// let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    /// let result = runtime.block_on(async {
    ///     Runner::new(1)?.call(script, "run".into(), vec![], CallOptions::default()).await
    /// })?;
    /// assert_eq!(result.value.as_int(), Some(21));
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    #[cfg(feature = "tokio")]
    pub fn new_async(
        name: impl Into<String>,
        callback: impl for<'a> Fn(
            &'a mut crate::asynchronous::AsyncHostCall,
            &'a [Value],
            &'a [(Value, Value)],
        ) -> crate::asynchronous::HostFuture<'a>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            definition: Arc::new(Definition {
                name: name.into(),
                callback: Callback::Async(Arc::new(callback)),
                arguments: None,
                result: None,
                signature: None,
            }),
        }
    }

    /// Installs validators bound to this method's identity, independently of its name.
    ///
    /// Argument validation precedes the callback; return validation runs on every
    /// successful result after import into the receiving budget. A failed callback
    /// has no result to validate. Cancellation and latched exhaustion take precedence
    /// over errors returned or ignored by either validator or the callback.
    pub fn with_contract(
        self,
        arguments: impl Fn(&mut CallContext, &[Value], &[(Value, Value)]) -> Result<()>
        + Send
        + Sync
        + 'static,
        result: impl Fn(&mut CallContext, &Value) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.with_block_contract(
            move |ctx, args, keywords, _| arguments(ctx, args, keywords),
            result,
        )
    }

    /// Installs contracts whose argument validator also receives block presence.
    ///
    /// Validation and exhaustion follow [`Self::with_contract`]. A `break` from
    /// the attached block becomes the method's result and passes return validation;
    /// a nonlocal `return` validates at its defining script method instead.
    pub fn with_block_contract(
        mut self,
        arguments: impl Fn(&mut CallContext, &[Value], &[(Value, Value)], bool) -> Result<()>
        + Send
        + Sync
        + 'static,
        result: impl Fn(&mut CallContext, &Value) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        let definition = Arc::make_mut(&mut self.definition);
        definition.arguments = Some(Arc::new(arguments));
        definition.result = Some(Arc::new(result));
        self
    }

    /// Publishes and enforces a positional signature using script annotation syntax.
    ///
    /// Invalid annotations and required parameters after optional ones are rejected
    /// immediately. Named types resolve in the calling source, including required
    /// files and defaults. Custom argument contracts see the original arguments;
    /// callbacks and return contracts receive normalized values. An absorbed block
    /// `break` also passes result validation. Signatures do not validate block inputs.
    pub fn with_signature(mut self, signature: crate::Signature) -> Result<Self> {
        let signature = crate::signature::Compiled::new(&self.definition.name, signature)?;
        Arc::make_mut(&mut self.definition).signature = Some(signature);
        Ok(self)
    }

    /// Returns the immutable published contract, if one was supplied.
    pub fn signature(&self) -> Option<&crate::Signature> {
        self.definition
            .signature
            .as_ref()
            .map(|signature| &signature.source)
    }

    /// Supplies immutable compiled contracts to the internal checker.
    pub(crate) fn compiled_signature(&self) -> Option<Arc<crate::signature::Compiled>> {
        self.definition.signature.clone()
    }

    /// Reports whether this method can invoke an attached script block.
    pub(crate) fn supports_block(&self) -> bool {
        self.definition.supports_block()
    }

    /// Creates a host-owned descriptor for a capability binding or object.
    ///
    /// Importing this descriptor grants it to that invocation. A descriptor already
    /// returned from a script keeps its original grant and cannot authorize another
    /// call. Reuse this host-side method to make a fresh grant instead.
    pub fn value(&self) -> Value {
        Value(Kind::Host(Arc::new(BoundMethod {
            definition: self.definition.clone(),
            owner: None,
            header: None,
            metadata: None,
        })))
    }
}

impl fmt::Debug for HostMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.definition.fmt(f)
    }
}

#[derive(Clone)]
pub(crate) struct Definition {
    name: String,
    callback: Callback,
    arguments: Option<ArgumentContract>,
    result: Option<ReturnContract>,
    signature: Option<Arc<crate::signature::Compiled>>,
}

impl Definition {
    fn supports_block(&self) -> bool {
        match self.callback {
            Callback::Plain(_) => false,
            Callback::Block(_) => true,
            #[cfg(feature = "tokio")]
            Callback::Async(_) => true,
        }
    }
}

impl fmt::Debug for Definition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostMethod")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub(crate) struct SelectedMethod {
    pub method: Arc<BoundMethod>,
    pub receiver: Value,
}

#[derive(Debug)]
pub(crate) struct BoundMethod {
    definition: Arc<Definition>,
    owner: Option<Weak<crate::budget::Memory>>,
    header: Option<Charge>,
    metadata: Option<Arc<Charge>>,
}

pub(crate) struct Root {
    definition: Arc<Definition>,
    metadata: Option<Arc<Charge>>,
}

impl BoundMethod {
    /// Reports whether a descriptor can grant a method to a new invocation.
    pub(crate) fn fresh_grant(&self) -> bool {
        self.owner.is_none()
    }

    pub fn name(&self) -> &str {
        &self.definition.name
    }

    pub fn value_error(&self) -> Error {
        Error::new(
            ErrorKind::Type,
            format!(
                "{} is a method and cannot be used as a value; call it through its capability",
                self.name(),
            ),
        )
    }

    pub fn import(ctx: &mut CallContext, value: &Arc<Self>) -> Result<Arc<Self>> {
        ctx.has_exports = true;
        if ctx.owns(&value.header) {
            return Ok(value.clone());
        }
        ctx.work_bytes(value.name().len())?;
        let metadata = retain(ctx, &value.definition, &value.metadata)?;
        let header = ctx.reserve(size_of::<Self>() + 2 * size_of::<usize>())?;
        Ok(Arc::new(Self {
            definition: value.definition.clone(),
            owner: Some(
                value
                    .owner
                    .clone()
                    .unwrap_or_else(|| Arc::downgrade(&ctx.identity())),
            ),
            header,
            metadata,
        }))
    }

    pub fn same(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.definition, &other.definition)
            && match (&self.owner, &other.owner) {
                (Some(left), Some(right)) => Weak::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
    }

    pub fn invoke_plain(
        &self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: &[(Value, Value)],
    ) -> Result<Value> {
        let Callback::Plain(callback) = &self.definition.callback else {
            unreachable!()
        };
        let result = callback(ctx, args, keywords);
        ctx.checkpoint()?;
        result
    }

    pub fn supports_block(&self) -> bool {
        self.definition.supports_block()
    }

    pub fn needs_frame(&self) -> bool {
        self.supports_block() || self.definition.signature.is_some()
    }

    pub fn signature(&self) -> Option<&crate::signature::Compiled> {
        self.definition.signature.as_deref()
    }

    /// Retains compiled checker metadata without retaining the callback or its grant.
    pub fn compiled_signature(&self) -> Option<Arc<crate::signature::Compiled>> {
        self.definition.signature.clone()
    }

    pub fn invoke(
        &self,
        call: &mut crate::HostCall<'_>,
        args: &[Value],
        keywords: &[(Value, Value)],
    ) -> Result<Value> {
        let result = match &self.definition.callback {
            Callback::Plain(callback) => callback(call.context(), args, keywords),
            Callback::Block(callback) => callback(call, args, keywords),
            #[cfg(feature = "tokio")]
            Callback::Async(_) => Err(Error::new(
                ErrorKind::Host,
                format!("{} requires asynchronous::Runner", self.name()),
            )),
        };
        call.context().checkpoint()?;
        result
    }

    #[cfg(feature = "tokio")]
    pub fn is_async(&self) -> bool {
        matches!(self.definition.callback, Callback::Async(_))
    }

    #[cfg(feature = "tokio")]
    pub fn invoke_async<'a>(
        &'a self,
        call: &'a mut crate::asynchronous::AsyncHostCall,
        args: &'a [Value],
        keywords: &'a [(Value, Value)],
    ) -> crate::asynchronous::HostFuture<'a> {
        let Callback::Async(callback) = &self.definition.callback else {
            unreachable!()
        };
        callback(call, args, keywords)
    }

    pub fn begin(
        &self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: &[(Value, Value)],
        block: bool,
    ) -> Result<()> {
        ctx.checkpoint()?;
        if !self
            .owner
            .as_ref()
            .is_some_and(|owner| Weak::ptr_eq(owner, &Arc::downgrade(&ctx.identity())))
        {
            return Err(Error::new(
                ErrorKind::Runtime,
                format!("capability {} was not granted to this call", self.name()),
            ));
        }
        if block && !self.supports_block() && self.signature().is_none() {
            return Err(Error::argument(format!(
                "{} does not accept a block",
                self.name()
            )));
        }
        if let Some(validate) = &self.definition.arguments {
            let result = validate(ctx, args, keywords, block);
            ctx.checkpoint()?;
            result?;
        }
        ctx.checkpoint()
    }

    pub fn finish(&self, ctx: &mut CallContext, value: Value) -> Result<Value> {
        let mut value = ctx.import(&value)?;
        if let Some(validate) = &self.definition.result {
            let result = validate(ctx, &value);
            ctx.checkpoint()?;
            result?;
            // A validator may retain its argument. Script mutations must not
            // modify that retained copy after this boundary returns.
            value = ctx.snapshot(&value)?;
        }
        crate::exports::check(ctx, &value)?;
        Ok(value)
    }
}

fn retain(
    ctx: &mut CallContext,
    definition: &Arc<Definition>,
    previous: &Option<Arc<Charge>>,
) -> Result<Option<Arc<Charge>>> {
    let mut roots = ctx.host_roots.take();
    // Trusted callback destructors must run outside the invocation's heap locks.
    let result = (|| {
        if let Some(roots) = &roots {
            for root in &roots.data {
                ctx.charge(1)?;
                if Arc::ptr_eq(&root.definition, definition) {
                    return Ok(root.metadata.clone());
                }
            }
        }
        let metadata = if previous
            .as_deref()
            .is_some_and(|charge| ctx.owns_charge(charge))
        {
            previous.clone()
        } else {
            let signature_bytes = definition
                .signature
                .as_ref()
                .map_or(0, |signature| signature.bytes);
            if signature_bytes > 0 {
                ctx.work_bytes(signature_bytes)?;
            }
            ctx.reserve(
                size_of::<Definition>()
                    + definition.name.capacity()
                    + signature_bytes
                    + size_of::<Charge>()
                    + 4 * size_of::<usize>(),
            )?
            .map(Arc::new)
        };
        if let Some(roots) = &mut roots {
            roots.push(
                ctx,
                Root {
                    definition: definition.clone(),
                    metadata: metadata.clone(),
                },
            )?;
        }
        Ok(metadata)
    })();
    ctx.host_roots = roots;
    result
}
