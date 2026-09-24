//! An experimental, embeddable Vibescript core with cooperative execution limits.
//!
//! ```
//! use vibescript::{Engine, Value, CallOptions};
//! let script = Engine::new().compile("def add(a, b)\n a + b\nend")?;
//! let result = script.call("add", &[Value::int(20), Value::int(22)], CallOptions::default())?;
//! assert_eq!(result.value.as_int(), Some(42));
//! # Ok::<(), vibescript::Error>(())
//! ```

#[cfg(all(target_os = "wasi", not(target_feature = "atomics"), feature = "tokio"))]
compile_error!(
    "the Tokio runner requires OS threads; build the WASI core without the tokio feature"
);

mod address;
mod arguments;
#[cfg(feature = "tokio")]
pub mod asynchronous;
mod budget;
mod builtin;
mod bytecode;
mod capability;
mod casing;
mod checking;
mod code;
mod collections;
mod combinatorics;
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
mod outline;
mod output;
mod pairs;
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
pub use checking::{CheckDiagnostic, CheckReport, CheckedOutcome};
pub use error::{Diagnostic, Error, ErrorClass, ErrorKind, Position, Result, StackFrame};
pub use host_call::HostCall;
pub use outline::{FunctionOutline, Outline, StatementKind, Unreachable};
pub use signature::{Signature, SignatureParam};
use std::{collections::BTreeMap, sync::Arc};
pub use value::Value;

// Compiles the session guide's examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../docs/sessions.md")]
struct SessionsGuide;

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
    /// Arguments and results are isolated snapshots of instance and module state,
    /// so retaining a value does not expose later script mutations.
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

/// What a top-level [`Declaration`] declares.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum DeclarationKind {
    /// A `def`, including `private def`, `export def` and a top-level `alias`.
    Function,
    Class,
    Module,
    Enum,
}

/// A top-level declaration in the source a [`Script`] was compiled from.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Declaration {
    pub kind: DeclarationKind,
    /// The declared name, such as `total`, `Invoice` or `Status`.
    pub name: String,
    /// The byte range of the declaration in the compiled source, from its first
    /// keyword, including `private` or `export`, through its final token.
    pub span: std::ops::Range<usize>,
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
        vm::Execution::call(self, name, args, keywords, options)
    }
    /// Runs top-level executable statements.
    pub fn run(&self, options: CallOptions) -> Result<Outcome> {
        self.call("__main__", &[], options)
    }
    /// Lists the top-level function, class, module and enum declarations in source order.
    ///
    /// Each span covers the declaration's source text, so a host can carry the
    /// declarations of one script into source it compiles later, as an
    /// interactive shell does. Declarations nested in other code are not listed.
    ///
    /// ```
    /// use vibescript::{DeclarationKind, Engine};
    /// let source = "x = 1\ndef double(n)\n  n * 2\nend\ndouble(x)";
    /// let script = Engine::new().compile(source)?;
    /// let declaration = &script.declarations()[0];
    /// assert_eq!(declaration.kind, DeclarationKind::Function);
    /// assert_eq!(declaration.name, "double");
    /// assert_eq!(&source[declaration.span.clone()], "def double(n)\n  n * 2\nend");
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn declarations(&self) -> &[Declaration] {
        &self.inner.code.program.outline
    }
    /// Runs top-level executable statements and returns the root bindings they leave.
    ///
    /// The bindings hold every entry of [`CallOptions::globals`], with the value the
    /// run left in it, the classes, modules and enums the script declares at the top
    /// level, and every top-level local the statements assigned. A local takes
    /// precedence over a global of the same name, and a supplied global shadows a
    /// declaration, as it does during the run. Values follow the result's isolation
    /// contract: they are snapshots that later calls cannot change, and passing them
    /// back as globals continues a session, as an interactive shell does; instances
    /// then still belong to the classes passed with them. Functions are not values,
    /// so they are not bindings; see [`Self::declarations`] to carry them.
    ///
    /// ```
    /// use vibescript::{CallOptions, Engine};
    /// let engine = Engine::new();
    /// let (_, bindings) = engine.compile("total = 40")?.run_bindings(CallOptions::default())?;
    /// let options = CallOptions { globals: bindings, ..CallOptions::default() };
    /// let (outcome, bindings) = engine.compile("total += 2")?.run_bindings(options)?;
    /// assert_eq!(outcome.value.as_int(), Some(42));
    /// assert_eq!(bindings["total"].as_int(), Some(42));
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn run_bindings(&self, options: CallOptions) -> Result<(Outcome, BTreeMap<String, Value>)> {
        vm::Execution::new(self, "__main__", &[], &[], options)?.run_bindings()
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

/// Returns the core builtins that every script can reach by name.
///
/// Builtin functions such as `puts` map to builtin descriptors, and namespaces
/// such as `JSON` and `Math` map to objects whose fields are their members,
/// including constants such as `Math::PI`. Tools list and complete names from
/// this map instead of keeping their own tables. It describes the language, so
/// registered host functions and capabilities are not included. Scripts still
/// cannot hold a descriptor as a value; a host may pass one back as a global.
pub fn builtins() -> BTreeMap<String, Value> {
    builtin::Global::ALL
        .iter()
        .map(|global| (global.name().to_owned(), global.value()))
        .collect()
}
