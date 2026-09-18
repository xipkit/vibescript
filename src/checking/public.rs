use super::{entry, report};
use crate::{
    CallContext, CallOptions, Outcome, Position, Result, Script, Stats, Value, budget::Charge,
};
use std::{fmt, sync::Arc};

/// A static diagnostic with an owned message and source location.
///
/// Locations identify the containing expression, not a runtime stack trace.
/// Retaining a diagnostic does not retain code, arguments or host callbacks.
#[derive(Debug)]
pub struct CheckDiagnostic {
    pub function: String,
    pub filename: Option<Arc<[u8]>>,
    pub offset: usize,
    pub position: Position,
    pub message: String,
    pub code_frame: String,
    pub(super) _source: super::sources::SourceId,
    pub(super) _charge: Option<Charge>,
}

impl fmt::Display for CheckDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n{}", self.message, self.code_frame)
    }
}

/// The result of checking one selected scope without executing script or host code.
///
/// Known contradictions and unimplemented analysis paths are reported separately.
/// Both lists are sorted by source and position and deduplicated within each source.
/// Unknown dynamic values alone do not make a check incomplete. Counters include report storage.
#[derive(Debug)]
pub struct CheckReport {
    pub diagnostics: Vec<CheckDiagnostic>,
    pub incomplete: Vec<CheckDiagnostic>,
    pub stats: Stats,
    pub(super) _charge: Option<Charge>,
}

impl CheckReport {
    /// Returns whether analysis finished without any known contradictions.
    ///
    /// A clean result is not proof of type safety; gradual values retain their
    /// runtime contracts and execution can still fail.
    pub fn is_clean(&self) -> bool {
        self.diagnostics.is_empty() && self.incomplete.is_empty()
    }
}

/// A checked call either executed or stopped before any script or host effects.
#[derive(Debug)]
pub enum CheckedOutcome {
    Executed(Outcome),
    Rejected(CheckReport),
}

impl Script {
    /// Checks top-level code, namespace bodies and every callable declaration without execution.
    ///
    /// Functions and methods use their declared parameter domains and both block
    /// presence paths. Unused declarations are included. Unknown block results
    /// remain gradual; ordinary calls still report known missing blocks.
    /// Namespace state follows top-level initialization. Constructor analysis retains
    /// possible unset fields when checking typed instance methods and class inputs.
    /// All scopes share the supplied work, memory, cancellation and deadline limits.
    ///
    /// ```
    /// use vibescript::{CallOptions, Engine};
    /// let script = Engine::new().compile("7;def unused -> int;false;end")?;
    /// assert!(!script.check(&CallOptions::default())?.is_clean());
    /// assert!(script.check_call("__main__", &[], &CallOptions::default())?.is_clean());
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn check(&self, options: &CallOptions) -> Result<CheckReport> {
        let mut ctx = self.checking_context(options);
        let mut report = super::whole::check(&mut ctx, self, options)?;
        ctx.checkpoint()?;
        report.stats = ctx.stats();
        Ok(report)
    }

    /// Checks the reachable calls of a named function without concrete arguments.
    ///
    /// Annotated parameters enter with their declared value domains; unannotated
    /// parameters remain gradual. Optional defaults are included in the analysis.
    /// Select instance methods with `Class#method`, static or module methods with
    /// `Namespace.method`, and constructors with `Class.new`. These are declaration
    /// selectors for checking, not names accepted by [`Self::call`]. Instance
    /// methods begin with unknown receiver state, without calling a constructor.
    /// Namespace initialization follows ordinary named-call ordering. This does
    /// not check unrelated functions or provide whole-file validation, and it
    /// never executes initializers, callbacks or writers to discover their values.
    /// Like [`Self::call`], this entry does not supply a script block.
    pub fn check_function(&self, name: &str, options: &CallOptions) -> Result<CheckReport> {
        let mut ctx = self.checking_context(options);
        let checked = entry::check_function(&mut ctx, self, name, options)?;
        let mut report = report::build(&mut ctx, &self.inner.code.program, &checked)?;
        drop(checked);
        ctx.checkpoint()?;
        report.stats = ctx.stats();
        Ok(report)
    }

    fn checking_context(&self, options: &CallOptions) -> CallContext {
        let mut ctx = CallContext::new(CallOptions {
            globals: Default::default(),
            capabilities: Vec::new(),
            limits: options.limits.clone(),
            cancellation: options.cancellation.clone(),
            deadline: options.deadline,
            allow_require: options.allow_require,
        });
        ctx.strict_effects = self.inner.strict_effects;
        ctx
    }

    /// Checks the reachable path of one concrete invocation without executing it.
    ///
    /// This does not check unused functions or provide whole-file validation.
    /// Factories, initializers and other unfinished paths appear in `incomplete`.
    /// Input guards, cancellation and exhausted analysis limits return an error.
    pub fn check_call(
        &self,
        name: &str,
        args: &[Value],
        options: &CallOptions,
    ) -> Result<CheckReport> {
        self.check_call_with_keywords(name, args, &[], options)
    }

    /// Checks a concrete call with named host keywords and isolated input facts.
    ///
    /// Every supplied value is checked, including earlier duplicate keywords.
    /// Repeated names bind their last value, just as in `call_with_keywords`.
    pub fn check_call_with_keywords(
        &self,
        name: &str,
        args: &[Value],
        keywords: &[(String, Value)],
        options: &CallOptions,
    ) -> Result<CheckReport> {
        let mut ctx = self.checking_context(options);
        let checked = entry::check(
            &mut ctx,
            entry::Call {
                script: self,
                name,
                arguments: args,
                keywords,
                options,
            },
        )?;
        let mut report = report::build(&mut ctx, &self.inner.code.program, &checked)?;
        drop(checked);
        ctx.checkpoint()?;
        report.stats = ctx.stats();
        Ok(report)
    }

    /// Executes a concrete call only when its static report is clean.
    ///
    /// Rejection returns the report without running defaults, initializers or
    /// callbacks. Analysis and execution each receive the supplied limits and
    /// share the cancellation token and absolute deadline. Execution errors
    /// remain ordinary errors; inspect the report first to retain clean-check
    /// counters independently of execution counters.
    ///
    /// ```
    /// use vibescript::{CallOptions, CheckedOutcome, Engine, Value};
    /// let script = Engine::new().compile("def add(n:int)->int;n+1;end")?;
    /// let args = [Value::int(41)];
    /// assert!(script.check_call("add", &args, &CallOptions::default())?.is_clean());
    /// match script.checked_call("add", &args, CallOptions::default())? {
    ///     CheckedOutcome::Executed(outcome) => assert_eq!(outcome.value.as_int(), Some(42)),
    ///     CheckedOutcome::Rejected(report) => panic!("{report:?}"),
    /// }
    /// # Ok::<(), vibescript::Error>(())
    /// ```
    pub fn checked_call(
        &self,
        name: &str,
        args: &[Value],
        options: CallOptions,
    ) -> Result<CheckedOutcome> {
        self.checked_call_with_keywords(name, args, &[], options)
    }

    /// Checks and conditionally executes the same positional and keyword inputs.
    pub fn checked_call_with_keywords(
        &self,
        name: &str,
        args: &[Value],
        keywords: &[(String, Value)],
        options: CallOptions,
    ) -> Result<CheckedOutcome> {
        let report = self.check_call_with_keywords(name, args, keywords, &options)?;
        if !report.is_clean() {
            return Ok(CheckedOutcome::Rejected(report));
        }
        drop(report);
        self.call_with_keywords(name, args, keywords, options)
            .map(CheckedOutcome::Executed)
    }
}
