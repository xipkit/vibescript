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
mod casing;
mod collections;
mod conversion;
mod duration;
mod enums;
mod error;
mod hash;
mod hash_blocks;
mod integer;
mod iteration;
mod json;
mod math;
mod members;
mod money;
mod mutate;
mod namespace;
mod numeric;
mod ops;
mod ordering;
mod printable;
mod random;
mod range;
mod regex;
mod scan;
mod sequence;
mod sets;
mod shapes;
mod sort;
mod syntax;
mod text;
mod time;
mod types;
mod value;
mod vm;

pub use budget::{CallContext, CallOptions, CancellationToken, Limits, Stats};
pub use error::{Error, ErrorKind, Result};
use std::{collections::BTreeMap, sync::Arc};
pub use value::Value;

/// A synchronous trusted host callback. It must cooperate with the supplied context.
pub type HostFunction = Arc<dyn Fn(&mut CallContext, &[Value]) -> Result<Value> + Send + Sync>;

type HostCallback =
    Arc<dyn Fn(&mut CallContext, &[Value], &[(Value, Value)]) -> Result<Value> + Send + Sync>;

/// A compiler configured with explicitly registered host capabilities.
#[derive(Default)]
pub struct Engine {
    hosts: BTreeMap<String, HostCallback>,
    random_source: Option<random::Source>,
}
impl Engine {
    /// Creates an engine with core builtins and no external capabilities.
    pub fn new() -> Self {
        Self::default()
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
        self.hosts.insert(name.into(), Arc::new(function));
    }
    /// Compiles UTF-8 source, enforcing source-size and syntax-depth guards.
    pub fn compile(&self, source: &str) -> Result<Script> {
        let names = self.hosts.keys().cloned().collect();
        let program = bytecode::compile(source, names)?;
        let hosts = program
            .hosts
            .iter()
            .map(|n| self.hosts[n].clone())
            .collect();
        Ok(Script {
            inner: Arc::new(ScriptInner {
                program,
                hosts,
                random_source: self.random_source.clone(),
            }),
        })
    }
}

struct ScriptInner {
    program: bytecode::Program,
    hosts: Vec<HostCallback>,
    random_source: Option<random::Source>,
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
        ctx.random_source = self.inner.random_source.clone();
        ctx.checkpoint()?;
        let function = *self
            .inner
            .program
            .names
            .get(name)
            .ok_or_else(|| Error::new(ErrorKind::Name, format!("unknown function {name}")))?;
        let value = vm::execute(
            &self.inner.program,
            &self.inner.hosts,
            &mut ctx,
            function,
            args,
            keywords,
        )?;
        ctx.random = None;
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
