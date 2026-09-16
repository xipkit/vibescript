use crate::{CallContext, Error, ErrorKind, Result, Value, budget::Charge, value::Kind};
use std::{
    fmt,
    sync::{Arc, Weak},
};

type Binder = Arc<dyn Fn(&mut CallContext) -> Result<Value> + Send + Sync>;
type ArgumentContract =
    Arc<dyn Fn(&mut CallContext, &[Value], &[(Value, Value)]) -> Result<()> + Send + Sync>;
type ReturnContract = Arc<dyn Fn(&mut CallContext, &Value) -> Result<()> + Send + Sync>;

/// A host namespace granted explicitly to one script invocation.
///
/// The factory runs once before script initialization, under the receiving call's
/// context. Return an object containing data and [`HostMethod`] values, or a
/// method descriptor for a global callable. Cloning a capability shares its
/// factory; mutable per-call state belongs inside the factory.
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
    binder: Binder,
}

impl Capability {
    /// Creates a named per-call binding factory. Later grants replace earlier names.
    pub fn new(
        name: impl Into<String>,
        bind: impl Fn(&mut CallContext) -> Result<Value> + Send + Sync + 'static,
    ) -> Self {
        Self {
            name: name.into(),
            binder: Arc::new(bind),
        }
    }

    pub(crate) fn bind(&self, ctx: &mut CallContext) -> Result<Value> {
        ctx.checkpoint()?;
        let value = (self.binder)(ctx);
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

/// A synchronous host method with optional argument and return validation.
///
/// Methods accept positional and keyword arguments. They cannot be detached into
/// script values and currently reject attached blocks. Callbacks and validators
/// must cooperate with cancellation and account their work through the context.
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
                callback: Arc::new(callback),
                arguments: None,
                result: None,
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
        mut self,
        arguments: impl Fn(&mut CallContext, &[Value], &[(Value, Value)]) -> Result<()>
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
    callback: crate::HostCallback,
    arguments: Option<ArgumentContract>,
    result: Option<ReturnContract>,
}

impl fmt::Debug for Definition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostMethod")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
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

    pub fn call(
        &self,
        ctx: &mut CallContext,
        args: &[Value],
        keywords: &[(Value, Value)],
        block: bool,
    ) -> Result<Value> {
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
        if block {
            return Err(Error::argument(format!(
                "{} does not accept a block",
                self.name()
            )));
        }
        if let Some(validate) = &self.definition.arguments {
            let result = validate(ctx, args, keywords);
            ctx.checkpoint()?;
            result?;
        }
        ctx.checkpoint()?;
        let result = (self.definition.callback)(ctx, args, keywords);
        ctx.checkpoint()?;
        let value = ctx.import(&result?)?;
        if let Some(validate) = &self.definition.result {
            let result = validate(ctx, &value);
            ctx.checkpoint()?;
            result?;
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
            ctx.reserve(
                size_of::<Definition>()
                    + definition.name.capacity()
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
