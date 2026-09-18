//! An experimental, embeddable Vibescript core with cooperative execution limits.
//!
//! ```
//! use vibescript::{Engine, Value, CallOptions};
//! let script = Engine::new().compile("def add(a, b)\n a + b\nend")?;
//! let result = script.call("add", &[Value::int(20), Value::int(22)], CallOptions::default())?;
//! assert_eq!(result.value.as_int(), Some(42));
//! # Ok::<(), vibescript::Error>(())
//! ```

mod address;
mod arguments;
#[cfg(feature = "tokio")]
pub mod asynchronous;
mod budget;
mod builtin;
mod bytecode;
mod capability;
mod casing;
// The checker remains internal until its control-flow and scope analysis is complete.
#[cfg(test)]
mod checking;
mod code;
mod collections;
mod compilation;
mod conversion;
mod duration;
mod enums;
mod error;
mod exports;
mod format;
mod globals;
mod hash;
mod hash_blocks;
mod host_call;
mod integer;
mod iteration;
mod json;
mod loading;
pub use loading::ModuleConfig;
mod math;
mod members;
mod money;
mod mutate;
mod namespace;
mod numeric;
mod objects;
mod ops;
mod ordering;
mod output;
mod printable;
mod random;
mod range;
mod regex;
mod scan;
mod sequence;
mod sets;
mod shapes;
mod signature;
mod sort;
mod source;
mod syntax;
mod text;
mod time;
mod types;
mod value;
mod vm;

pub use budget::{CallContext, CallOptions, CancellationToken, Limits, Stats};
pub use capability::{Capability, HostMethod};
pub use error::{Diagnostic, Error, ErrorClass, ErrorKind, Position, Result, StackFrame};
pub use host_call::HostCall;
pub use signature::{Signature, SignatureParam};
use std::{collections::BTreeMap, sync::Arc};
pub use value::Value;

/// A synchronous trusted host callback. It must cooperate with the supplied context.
pub type HostFunction = Arc<dyn Fn(&mut CallContext, &[Value]) -> Result<Value> + Send + Sync>;

type HostCallback =
    Arc<dyn Fn(&mut CallContext, &[Value], &[(Value, Value)]) -> Result<Value> + Send + Sync>;

/// A compiler configured with explicitly registered host capabilities.
#[derive(Default)]
pub struct Engine {
    hosts: BTreeMap<String, capability::Registered>,
    loader: Arc<loading::Loader>,
    strict_effects: bool,
    random_source: Option<random::Source>,
    output_writer: Option<output::Writer>,
    error_writer: Option<output::Writer>,
}
impl Engine {
    /// Creates an engine with core builtins and no external capabilities.
    pub fn new() -> Self {
        Self::default()
    }
    /// Requires data-only globals and per-call permission for `require` in new scripts.
    ///
    /// When enabled, callers must set [`CallOptions::allow_require`] before a script
    /// may load a module, including a cached module. Registered host callbacks remain
    /// available. All host globals are validated before script execution, including
    /// unused values. Earlier scripts retain their previous mode. Disabled by default.
    pub fn set_strict_effects(&mut self, enabled: bool) {
        self.strict_effects = enabled;
    }
    /// Configures required files for subsequently compiled scripts.
    ///
    /// Configured roots are opened immediately. Earlier scripts retain their previous
    /// loader; calls share compiled source but keep independent initialized state.
    pub fn set_module_config(&mut self, config: ModuleConfig) -> Result<()> {
        self.loader = Arc::new(loading::Loader::new(config)?);
        Ok(())
    }
    /// Clears this configuration's compiled module cache without changing active calls.
    pub fn clear_module_cache(&self) {
        self.loader.clear();
    }
    /// Sets the writer used by `puts`, `print`, and `p` in subsequently compiled scripts.
    ///
    /// The callback must write the entire byte slice or return an error. It may run
    /// concurrently and must cooperate with the supplied context's cancellation and
    /// deadlines. Bytes retained by the writer belong to the host. No writer is
    /// configured by default; output helpers then report a script error.
    pub fn set_output_writer(
        &mut self,
        writer: impl Fn(&mut CallContext, &[u8]) -> Result<()> + Send + Sync + 'static,
    ) {
        self.output_writer = Some(Arc::new(writer));
    }
    /// Sets the writer used by `warn` in subsequently compiled scripts.
    ///
    /// It follows the same full-write and cooperation contract as [`Self::set_output_writer`].
    pub fn set_error_writer(
        &mut self,
        writer: impl Fn(&mut CallContext, &[u8]) -> Result<()> + Send + Sync + 'static,
    ) {
        self.error_writer = Some(Arc::new(writer));
    }
    /// Sets the entropy reader used by subsequently compiled scripts.
    ///
    /// The reader writes into the provided buffer and returns the number of bytes
    /// written. Partial reads are retried; zero or oversized counts are errors.
    /// It may run concurrently and must cooperate with cancellation and deadlines.
    /// Without a custom reader, the engine uses operating-system entropy.
    pub fn set_random_source(
        &mut self,
        reader: impl Fn(&mut CallContext, &mut [u8]) -> Result<usize> + Send + Sync + 'static,
    ) {
        self.random_source = Some(Arc::new(reader));
    }
    /// Registers a synchronous host function for subsequently compiled scripts.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        function: impl Fn(&mut CallContext, &[Value]) -> Result<Value> + Send + Sync + 'static,
    ) {
        self.register_with_keywords(name, move |ctx, args, keywords| {
            if !keywords.is_empty() {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "host function does not accept keyword arguments",
                ));
            }
            function(ctx, args)
        });
    }
    /// Registers a synchronous callback that accepts positional and keyword arguments.
    ///
    /// Keyword keys are byte-string values. Both argument collections are accounted to
    /// the current call; the callback validates its own names, types, and required values.
    pub fn register_with_keywords(
        &mut self,
        name: impl Into<String>,
        function: impl Fn(&mut CallContext, &[Value], &[(Value, Value)]) -> Result<Value>
        + Send
        + Sync
        + 'static,
    ) {
        self.hosts.insert(
            name.into(),
            capability::Registered::Callback(Arc::new(function)),
        );
        self.loader = Arc::new(self.loader.fresh());
    }

    /// Registers a host method, including its signature, contracts and block driver.
    ///
    /// Registration affects subsequently compiled scripts and their required files.
    /// Each invocation receives a fresh grant. Script declarations and explicit
    /// globals retain their normal lookup precedence.
    pub fn register_method(&mut self, name: impl Into<String>, method: HostMethod) {
        self.hosts
            .insert(name.into(), capability::Registered::Method(method));
        self.loader = Arc::new(self.loader.fresh());
    }
    /// Compiles UTF-8 source, enforcing source-size and syntax-depth guards.
    pub fn compile(&self, source: &str) -> Result<Script> {
        let code = code::Code::compile(source, &self.hosts)?;
        Ok(Script {
            inner: Arc::new(ScriptInner {
                code,
                loader: self.loader.clone(),
                strict_effects: self.strict_effects,
                random_source: self.random_source.clone(),
                output_writer: self.output_writer.clone(),
                error_writer: self.error_writer.clone(),
            }),
        })
    }
}

struct ScriptInner {
    code: Arc<code::Code>,
    loader: Arc<loading::Loader>,
    strict_effects: bool,
    random_source: Option<random::Source>,
    output_writer: Option<output::Writer>,
    error_writer: Option<output::Writer>,
}

/// Immutable compiled code, safely shared across independent calls and threads.
#[derive(Clone)]
pub struct Script {
    inner: Arc<ScriptInner>,
}
impl Script {
    /// Calls a named function with isolated arguments and fresh execution limits.
    pub fn call(&self, name: &str, args: &[Value], options: CallOptions) -> Result<Outcome> {
        self.call_with_keywords(name, args, &[], options)
    }
    /// Calls a named function with isolated positional and keyword arguments.
    ///
    /// Repeated keyword names use the last value. Host-supplied keywords bind by name;
    /// they do not collapse into a trailing positional options hash.
    pub fn call_with_keywords(
        &self,
        name: &str,
        args: &[Value],
        keywords: &[(String, Value)],
        options: CallOptions,
    ) -> Result<Outcome> {
        let mut ctx = CallContext::new(options);
        ctx.strict_effects = self.inner.strict_effects;
        ctx.random_source = self.inner.random_source.clone();
        ctx.output_writer = self.inner.output_writer.clone();
        ctx.error_writer = self.inner.error_writer.clone();
        ctx.checkpoint()?;
        let function = *self
            .inner
            .code
            .program
            .names
            .get(name)
            .ok_or_else(|| Error::new(ErrorKind::Name, format!("unknown function {name}")))?;
        ctx.code_roots = Some(budget::Buffer::empty());
        ctx.host_roots = Some(budget::Buffer::empty());
        let result = vm::execute(
            &self.inner.code,
            &self.inner.loader,
            &mut ctx,
            function,
            args,
            keywords,
        );
        ctx.random = None;
        let value = match result {
            Ok(value) => value,
            Err(error) => {
                objects::cleanup(&mut ctx);
                ctx.code_roots = None;
                ctx.host_roots = None;
                return Err(error);
            }
        };
        if let Err(error) = objects::finish(&mut ctx) {
            drop(value);
            objects::cleanup(&mut ctx);
            ctx.code_roots = None;
            ctx.host_roots = None;
            return Err(error);
        }
        ctx.code_roots = None;
        ctx.host_roots = None;
        ctx.checkpoint()?;
        Ok(Outcome {
            value,
            stats: ctx.stats(),
        })
    }
    /// Runs top-level executable statements.
    pub fn run(&self, options: CallOptions) -> Result<Outcome> {
        self.call("__main__", &[], options)
    }
}

/// A completed call and its counters after interpreter frames and scratch storage are released.
#[derive(Debug)]
pub struct Outcome {
    pub value: Value,
    pub stats: Stats,
}

/// Parses JSON with accounted storage under an independent execution budget.
pub fn parse_json(input: &[u8], options: CallOptions) -> Result<Outcome> {
    let mut ctx = CallContext::new(options);
    let value = json::parse(&mut ctx, input)?;
    Ok(Outcome {
        value,
        stats: ctx.stats(),
    })
}

/// Encodes a host value as a JSON byte string under an independent execution budget.
pub fn stringify_json(value: &Value, options: CallOptions) -> Result<Outcome> {
    let mut ctx = CallContext::new(options);
    let imported = ctx.import(value)?;
    let value = json::stringify(&mut ctx, &imported)?;
    drop(imported);
    Ok(Outcome {
        value,
        stats: ctx.stats(),
    })
}
