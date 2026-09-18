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
    pub(super) _charge: Option<Charge>,
}

impl fmt::Display for CheckDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}\n{}", self.message, self.code_frame)
    }
}

/// The result of checking one concrete call without executing script or host code.
///
/// Known contradictions and unimplemented analysis paths are reported separately.
/// Both lists are sorted by source position and deduplicated. Unknown dynamic
/// values alone do not make a check incomplete. Counters include report storage.
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
        let mut ctx = CallContext::new(CallOptions {
            globals: Default::default(),
            capabilities: Vec::new(),
            limits: options.limits.clone(),
            cancellation: options.cancellation.clone(),
            deadline: options.deadline,
            allow_require: options.allow_require,
        });
        ctx.strict_effects = self.inner.strict_effects;
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
