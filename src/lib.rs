//! An experimental, embeddable Vibescript core with cooperative execution limits.
//!
//! ```
//! use vibescript::{Engine, Value, CallOptions};
//! let script = Engine::new().compile("def add(a: int, b: int) -> int\n a + b\nend")?;
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
mod declared;
pub mod diagnostic;
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
#[cfg(feature = "observe")]
pub mod observe;
mod ops;
mod ordering;
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
pub mod signatures;
mod sort;
mod source;
pub mod surface;
mod syntax;
mod text;
mod time;
pub mod tooling;
mod types;
pub mod typing;
mod value;
mod vm;

pub use budget::{CallContext, CallOptions, CancellationToken, Limits, Stats};
pub use capability::{Capability, HostMethod};
pub use checking::{CheckDiagnostic, CheckReport, CheckedOutcome};
pub use error::{Diagnostic, Error, ErrorClass, ErrorKind, Position, Result, StackFrame};
pub use host_call::HostCall;
pub use signature::{Signature, SignatureParam};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
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
pub struct Engine {
    hosts: BTreeMap<String, capability::Registered>,
    /// Hosts registered with [`Self::register`], which refuse keywords.
    keywordless: BTreeSet<String>,
    /// Globals and capabilities every call supplies, with their types.
    declared: Arc<declared::Declarations>,
    loader: Arc<loading::Loader>,
    strict_effects: bool,
    static_types: bool,
    random_source: Option<random::Source>,
    output_writer: Option<output::Writer>,
    error_writer: Option<output::Writer>,
    #[cfg(feature = "observe")]
    observer: Option<Arc<dyn observe::Observer>>,
}

/// Whether engines type check statically unless told otherwise. Building
/// with the `VIBESCRIPT_STATIC_TYPES` environment variable set turns it on,
/// so the test suite can run as it will once static types are the only
/// mode. This crate's own unit tests opt in through `test_engine`, since
/// the gradual checker's tests keep running the ADR-004 language until
/// that checker is removed.
#[doc(hidden)]
pub const STATIC_TYPES_BY_DEFAULT: bool = option_env!("VIBESCRIPT_STATIC_TYPES").is_some();

/// An engine for this crate's unit tests that follows
/// [`STATIC_TYPES_BY_DEFAULT`].
#[cfg(test)]
pub(crate) fn test_engine() -> Engine {
    let mut engine = Engine::new();
    engine.set_static_types(STATIC_TYPES_BY_DEFAULT);
    engine
}

impl Default for Engine {
    fn default() -> Self {
        Self {
            hosts: BTreeMap::new(),
            keywordless: BTreeSet::new(),
            declared: Arc::default(),
            loader: Arc::default(),
            strict_effects: false,
            static_types: STATIC_TYPES_BY_DEFAULT && !cfg!(test),
            random_source: None,
            output_writer: None,
            error_writer: None,
            #[cfg(feature = "observe")]
            observer: None,
        }
    }
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
    /// Type checks subsequently compiled scripts and the files they require
    /// statically (ADR-007).
    ///
    /// When enabled, a program with type errors does not compile: the error's
    /// [`Error::diagnostics`] lists every type error with its code, spans,
    /// expected and found types and fixes. Disabled by default until the
    /// static checker replaces the gradual one; the setting then goes away.
    ///
    /// ```
    /// let mut engine = vibescript::Engine::new();
    /// engine.set_static_types(true);
    /// let script = engine.compile("def add(a: int, b: int) -> int\n  a + b\nend\n")?;
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn set_static_types(&mut self, enabled: bool) {
        self.static_types = enabled;
        self.loader = Arc::new(self.loader.fresh());
    }
    /// Type checks `source` without compiling it, as [`Self::set_static_types`]
    /// does, returning every diagnostic and the static receiver type of each
    /// member call. Only a syntax error fails.
    ///
    /// ```
    /// let checked = vibescript::Engine::new().type_check("def size(items: array<int>) -> int\n  items.length\nend\n")?;
    /// assert!(checked.diagnostics.is_empty());
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn type_check(&self, source: &str) -> Result<typing::Checked> {
        let (parsed, tokens) = syntax::parse_with_tokens(source, &())
            .map_err(|error| source::parse_error(source, None, error, &()))?;
        let resolve = |path: &str| self.loader.source(path);
        Ok(typing::check(&typing::Input {
            source,
            parsed: &parsed,
            tokens: &tokens,
            hosts: self.hosts.iter().collect(),
            declared: &self.declared,
            file: false,
            modules: Some(&resolve),
        }))
    }
    /// Checks that a command line can call `function` in `source` with
    /// `count` arguments, which it passes as strings (ADR-007): each
    /// positional parameter they bind must accept `string`, and a rest
    /// parameter `array<string>`. Returns the diagnostics; only a syntax
    /// error fails.
    ///
    /// ```
    /// let engine = vibescript::Engine::new();
    /// let source = "def run(name: string, times: int) -> string\n  name * times\nend\n";
    /// let found = engine.check_entry_arguments(source, "run", 2)?;
    /// assert_eq!(found.len(), 1);
    /// assert!(found[0].message.contains("`times` of `run` is int"));
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn check_entry_arguments(
        &self,
        source: &str,
        function: &str,
        count: usize,
    ) -> Result<Vec<diagnostic::Diagnostic>> {
        let (parsed, tokens) = syntax::parse_with_tokens(source, &())
            .map_err(|error| source::parse_error(source, None, error, &()))?;
        Ok(typing::entry_arguments(
            &typing::Input {
                source,
                parsed: &parsed,
                tokens: &tokens,
                hosts: self.hosts.iter().collect(),
                declared: &self.declared,
                file: false,
                modules: None,
            },
            function,
            count,
        ))
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
    /// Reports the values that subsequently compiled scripts compute to `observer`.
    ///
    /// Calls then run every instruction through the general dispatch, so
    /// they are slower; their results and accounting are unchanged.
    #[cfg(feature = "observe")]
    pub fn set_observer(&mut self, observer: Arc<dyn observe::Observer>) {
        self.observer = Some(observer);
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
        let name = name.into();
        self.register_with_keywords(name.clone(), move |ctx, args, keywords| {
            if !keywords.is_empty() {
                return Err(Error::new(
                    ErrorKind::Argument,
                    "host function does not accept keyword arguments",
                ));
            }
            function(ctx, args)
        });
        self.keywordless.insert(name);
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
        let name = name.into();
        self.keywordless.remove(&name);
        self.hosts
            .insert(name, capability::Registered::Callback(Arc::new(function)));
        self.loader = Arc::new(self.loader.fresh());
    }

    /// Registers a host method, including its signature, contracts and block driver.
    ///
    /// Registration affects subsequently compiled scripts and their required files.
    /// Each invocation receives a fresh grant. Script declarations and explicit
    /// globals retain their normal lookup precedence.
    pub fn register_method(&mut self, name: impl Into<String>, method: HostMethod) {
        let name = name.into();
        self.keywordless.remove(&name);
        self.hosts
            .insert(name, capability::Registered::Method(method));
        self.loader = Arc::new(self.loader.fresh());
    }

    /// Declares a global that every call supplies in
    /// [`CallOptions::globals`], with the type its value has, written as an
    /// annotation such as `{ id: string, plan: string }`. An empty `ty`
    /// declares a value of type `any`, which a script narrows before use.
    ///
    /// The static checker types the name by its declaration, where an
    /// undeclared name is an error, and [`Self::prelude`] lists it. Every
    /// call of a subsequently compiled script must supply a global or a
    /// capability of the name, and a global's value must have the declared
    /// type, or the call fails before any script code runs, as an argument
    /// of the wrong type would. A later declaration of the name replaces an
    /// earlier one. Host values cannot be script classes or enums, so the
    /// type uses builtin types only.
    ///
    /// ```
    /// use vibescript::{CallOptions, Engine, ErrorKind, Value};
    /// let mut engine = Engine::new();
    /// engine.set_static_types(true);
    /// engine.declare_global("limit", "int")?;
    /// let script = engine.compile("def doubled -> int\n  limit * 2\nend\n")?;
    /// let options = |value| CallOptions {
    ///     globals: [("limit".to_owned(), value)].into(),
    ///     ..CallOptions::default()
    /// };
    /// let outcome = script.call("doubled", &[], options(Value::int(21)))?;
    /// assert_eq!(outcome.value.as_int(), Some(42));
    /// let error = script.call("doubled", &[], options(Value::bytes("21"))).unwrap_err();
    /// assert_eq!(error.kind, ErrorKind::Type);
    /// assert_eq!(error.message, "global limit expected int, got string");
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn declare_global(&mut self, name: impl Into<String>, ty: &str) -> Result<()> {
        let name = name.into();
        let declaration = declared::Declaration::global(&name, ty)?;
        Arc::make_mut(&mut self.declared).insert(name, declaration);
        self.loader = Arc::new(self.loader.fresh());
        Ok(())
    }

    /// Declares a capability that every call grants, typed by its binding:
    /// the template of a capability made with [`Capability::from_value`]. A
    /// host method in it is typed by its published [`Signature`], or takes
    /// and returns `any` without one; an object holding host methods is a
    /// namespace of those methods and its data, each datum typed as its
    /// template value shows; and other data has the type its value shows. A
    /// capability made with [`Capability::new`] builds its value when a call
    /// starts, so it declares the name as `any`.
    ///
    /// The static checker and [`Self::prelude`] read the declaration. Every
    /// call of a subsequently compiled script must grant a capability, or
    /// supply a global, of the name whose value has the declared members:
    /// host methods with the declared signatures, and data of the declared
    /// types. Otherwise the call fails before any script code runs. A later
    /// declaration of the name replaces an earlier one.
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
    /// let mut engine = Engine::new();
    /// engine.set_static_types(true);
    /// engine.declare_capability(&sms)?;
    /// let script = engine.compile("def notify -> string\n  SMS.send(\"hello\")\nend\n")?;
    /// assert!(engine.compile("def notify -> string\n  SMS.send(1)\nend\n").is_err());
    /// let options = CallOptions { capabilities: vec![sms], ..CallOptions::default() };
    /// assert_eq!(script.call("notify", &[], options)?.value.as_bytes(), Some(b"queued".as_slice()));
    /// assert!(script.call("notify", &[], CallOptions::default()).is_err());
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn declare_capability(&mut self, capability: &Capability) -> Result<()> {
        let declaration = declared::Declaration::capability(capability)?;
        Arc::make_mut(&mut self.declared).insert(capability.name.clone(), declaration);
        self.loader = Arc::new(self.loader.fresh());
        Ok(())
    }

    /// Returns the builtin prelude extended with this host's declarations.
    ///
    /// The text is [`signatures::prelude`] followed by the functions
    /// registered on this engine, the globals and capabilities it declares
    /// ([`Self::declare_global`], [`Self::declare_capability`]), then the
    /// capabilities and globals that `options` grants a call under other
    /// names, each as a Vibescript declaration a model can read as context.
    /// Host methods render their published [`Signature`]; unsigned functions
    /// take and return `any`. An undeclared capability built from a template
    /// renders its methods and data, while an undeclared factory capability
    /// and every undeclared data global are `any`, since their values are
    /// known only when a call starts. The text parses as a
    /// [`signatures::Table`].
    ///
    /// ```
    /// use vibescript::{CallOptions, Engine, HostMethod, Signature, SignatureParam};
    /// let mut engine = Engine::new();
    /// let charge = HostMethod::new("charge", |ctx, _, _| ctx.bytes(b"ok"))
    ///     .with_signature(Signature {
    ///         params: vec![SignatureParam { name: "cents".into(), ty: "int".into(), optional: false }],
    ///         result: "string".into(),
    ///         accepts_block: false,
    ///     })?;
    /// engine.register_method("charge", charge);
    /// let prelude = engine.prelude(&CallOptions::default());
    /// assert!(prelude.starts_with(&vibescript::signatures::prelude()));
    /// assert!(prelude.ends_with("# A host function.\ndef charge(cents: int) -> string\n"));
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn prelude(&self, options: &CallOptions) -> String {
        signatures::host::table(&self.hosts, &self.keywordless, &self.declared, options).to_string()
    }
    /// Compiles UTF-8 source, enforcing source-size and syntax-depth guards.
    pub fn compile(&self, source: &str) -> Result<Script> {
        let code = code::Code::compile_metered(
            source,
            &self.hosts,
            &self.declared,
            &(),
            self.static_types.then_some(&*self.loader),
        )?;
        Ok(self.script(code))
    }

    /// Compiles like [`Self::compile`], charging the work to the step and memory
    /// quotas of `options` and stopping at its deadline or cancellation.
    ///
    /// Callers such as editors use this to bound compilation of untrusted or very
    /// large sources. An exhausted quota, the deadline or cancellation returns that
    /// error instead of a script. The script retains none of the budget, and
    /// globals and capabilities in `options` are ignored.
    ///
    /// ```
    /// use std::time::Instant;
    /// use vibescript::{CallOptions, Engine, ErrorKind, Limits};
    /// let engine = Engine::new();
    /// let options = CallOptions { deadline: Some(Instant::now()), ..CallOptions::default() };
    /// let error = engine.compile_with_options("def f; 1; end", &options).err().unwrap();
    /// assert_eq!(error.kind, ErrorKind::Deadline);
    /// let limits = Limits { steps: Some(1_000), ..Limits::default() };
    /// let options = CallOptions { limits, ..CallOptions::default() };
    /// assert!(engine.compile_with_options("def f; 1; end", &options).is_ok());
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn compile_with_options(&self, source: &str, options: &CallOptions) -> Result<Script> {
        let mut ctx = CallContext::new(CallOptions {
            globals: Default::default(),
            capabilities: Vec::new(),
            limits: options.limits.clone(),
            cancellation: options.cancellation.clone(),
            deadline: options.deadline,
            allow_require: options.allow_require,
        });
        let code = code::Code::compile_metered(
            source,
            &self.hosts,
            &self.declared,
            &compilation::Meter(std::cell::RefCell::new(&mut ctx)),
            self.static_types.then_some(&*self.loader),
        )?;
        ctx.checkpoint()?;
        Ok(self.script(code))
    }

    fn script(&self, code: Arc<code::Code>) -> Script {
        Script {
            inner: Arc::new(ScriptInner {
                code,
                loader: self.loader.clone(),
                strict_effects: self.strict_effects,
                random_source: self.random_source.clone(),
                output_writer: self.output_writer.clone(),
                error_writer: self.error_writer.clone(),
                #[cfg(feature = "observe")]
                observer: self.observer.clone(),
            }),
        }
    }
}

struct ScriptInner {
    code: Arc<code::Code>,
    loader: Arc<loading::Loader>,
    strict_effects: bool,
    random_source: Option<random::Source>,
    output_writer: Option<output::Writer>,
    error_writer: Option<output::Writer>,
    #[cfg(feature = "observe")]
    observer: Option<Arc<dyn observe::Observer>>,
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
    /// Returns the source text the script was compiled from.
    ///
    /// Tools can inspect it with [`tooling`] without compiling it again.
    ///
    /// ```
    /// let script = vibescript::Engine::new().compile("def run\n  1\nend\n")?;
    /// let outline = vibescript::tooling::outline(script.source())?;
    /// assert_eq!(outline.items[0].name, "run");
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn source(&self) -> &str {
        self.inner.code.program.source.text()
    }
    /// Lists the top-level function, class, module and enum declarations in source order.
    ///
    /// Each span covers the declaration's source text, so a host can carry the
    /// declarations of one script into source it compiles later, as an
    /// interactive shell does. Declarations nested in other code are not listed;
    /// [`tooling::outline`] describes members, signatures and positions, and
    /// works on source that has not compiled.
    ///
    /// ```
    /// use vibescript::{DeclarationKind, Engine};
    /// let source = "x = 1\ndef double(n: int) -> int\n  n * 2\nend\ndouble(x)";
    /// let script = Engine::new().compile(source)?;
    /// let declaration = &script.declarations()[0];
    /// assert_eq!(declaration.kind, DeclarationKind::Function);
    /// assert_eq!(declaration.name, "double");
    /// assert_eq!(&source[declaration.span.clone()], "def double(n: int) -> int\n  n * 2\nend");
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
    /// let (_, bindings) = Engine::new().compile("total = 40")?.run_bindings(CallOptions::default())?;
    /// // The next input reads `total` as a global the host supplies.
    /// let mut engine = Engine::new();
    /// engine.declare_global("total", "int")?;
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
