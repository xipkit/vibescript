//! An experimental, embeddable Vibescript core with cooperative execution limits.
//!
//! ```
//! use vibescript::{Engine, Value, CallOptions};
//! let script = Engine::new().compile("def add(a, b)\n a + b\nend")?;
//! let result = script.call("add", &[Value::int(20), Value::int(22)], CallOptions::default())?;
//! assert_eq!(result.value.as_int(), Some(42));
//! # Ok::<(), vibescript::Error>(())
//! ```

#[cfg(feature = "tokio")]
pub mod asynchronous;
mod budget;
mod bytecode;
mod error;
mod hash;
mod json;
mod ops;
mod scan;
mod syntax;
mod value;
mod vm;

pub use budget::{CallContext, CallOptions, CancellationToken, Limits, Stats};
pub use error::{Error, ErrorKind, Result};
use std::{collections::BTreeMap, sync::Arc};
pub use value::Value;

/// A synchronous trusted host callback. It must cooperate with the supplied context.
pub type HostFunction = Arc<dyn Fn(&mut CallContext, &[Value]) -> Result<Value> + Send + Sync>;

/// A compiler configured with explicitly registered host capabilities.
#[derive(Default)]
pub struct Engine {
    hosts: BTreeMap<String, HostFunction>,
}
impl Engine {
    /// Creates an engine with core builtins and no external capabilities.
    pub fn new() -> Self {
        Self::default()
    }
    /// Registers a synchronous host function for subsequently compiled scripts.
    pub fn register(
        &mut self,
        name: impl Into<String>,
        function: impl Fn(&mut CallContext, &[Value]) -> Result<Value> + Send + Sync + 'static,
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
            inner: Arc::new(ScriptInner { program, hosts }),
        })
    }
}

struct ScriptInner {
    program: bytecode::Program,
    hosts: Vec<HostFunction>,
}

/// Immutable compiled code, safely shared across independent calls and threads.
#[derive(Clone)]
pub struct Script {
    inner: Arc<ScriptInner>,
}
impl Script {
    /// Calls a named function with isolated arguments and fresh execution limits.
    pub fn call(&self, name: &str, args: &[Value], options: CallOptions) -> Result<Outcome> {
        let mut ctx = CallContext::new(options);
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
        )?;
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

/// Parses JSON with the same limits, decoder, and allocation rules used by scripts.
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
