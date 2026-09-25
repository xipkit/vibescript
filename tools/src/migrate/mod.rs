//! Rewrites Vibescript written for the ADR-004 language into the language of
//! ADR-007 (static types) and ADR-008 (a canonical surface), as `vibes
//! migrate` applies it.
//!
//! A migration makes three kinds of change:
//!
//! - **Mechanical rewrites** that need no types: removed spellings from
//!   [`vibescript::signatures::renames`], `do ... end` blocks to braces,
//!   `unless` and `until` to `if !` and `while !`, symbol hash keys to
//!   strings, percent literals to arrays, `Hash.new` to `{}`, `nil?` to
//!   `== nil`, empty argument parentheses, and dispatch by a literal name.
//! - **Type annotations** for parameters, results, yielded blocks, locals
//!   whose first value does not fix their type, and instance variables.
//!   Types come from existing annotations, then from values observed while
//!   running the program's recorded invocations ([`observe`]), and
//!   otherwise are `any`, which the report flags.
//! - **Semantic rewrites** where observed types decide the meaning: integer
//!   `/` becomes `//`, and a condition on an optional value becomes
//!   `!= nil`.
//!
//! Everything a migration cannot do safely is reported as a [`Diagnostic`]
//! instead of guessed. Comments and layout outside the rewritten tokens are
//! kept, and the result is in the formatter's canonical form.
//!
//! ```
//! use vibescript_tools::migrate::{Invocation, Options, migrate, observe};
//! let source = "def half(n)\n  n / 2 unless n.nil?\nend\n";
//! let call = serde_json::json!({"function": "half", "args": [7]});
//! let observations = observe(source, &[Invocation::from_json(call, ".".as_ref())?]);
//! let migration = migrate(source, &observations, &Options::default());
//! assert_eq!(migration.source, "def half(n: int) -> int\n  n // 2 if n != nil\nend\n");
//! # Ok::<(), String>(())
//! ```

mod annotate;
mod compat;
mod diff;
mod migrator;
mod observe;
mod semantics;
#[cfg(test)]
mod tests;
mod types;

pub use diff::unified_diff;
pub use observe::{Invocation, Observations, observe};

/// How far a migration goes.
#[derive(Clone, Debug)]
pub struct Options {
    /// Whether to write syntax that only the ADR-007 compiler accepts:
    /// typed locals, instance-variable declarations, block parameter types,
    /// typed optional keywords, `//` and bare zero-argument output calls.
    /// Without it, a migration makes only the changes today's runtime
    /// accepts, which checks every annotation it adds at runtime.
    pub new_syntax: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { new_syntax: true }
    }
}

/// The result of migrating one source.
#[derive(Clone, Debug)]
pub struct Migration {
    /// The migrated source, in the formatter's canonical form when it changed.
    pub source: String,
    pub changed: bool,
    /// Everything the migration could not do automatically.
    pub diagnostics: Vec<Diagnostic>,
}

/// Something a migration left for a person, or flagged for review.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Code,
    /// The byte offset the diagnostic points at.
    pub offset: usize,
    /// The one-based line and character column of `offset`.
    pub line: usize,
    pub column: usize,
    pub message: String,
}

/// Why a [`Diagnostic`] was reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Code {
    /// The source does not compile, so it was left alone.
    Unparsed,
    /// A removed spelling whose replacement needs a person (`manual:` in
    /// the rename table).
    Rename,
    /// A removed spelling whose receiver type is unknown or mixed, so the
    /// right replacement is not certain.
    Receiver,
    /// `send`, `public_send` or `respond_to?` with a name known only at runtime.
    Dispatch,
    /// A `require` whose path or alias is not a string literal.
    Require,
    /// `Hash.new` with a default or a block.
    HashNew,
    /// An annotation that fell back to `any`, which must be narrowed.
    Any,
    /// A `/` whose operands were not always integers or never observed.
    Division,
    /// A condition on a value that is not always a `bool` and cannot be
    /// rewritten as a nil test.
    Condition,
    /// A `case` over an enum that neither names every member nor has an `else`.
    Case,
    /// A local first assigned where its declaration cannot go, such as
    /// inside a block.
    Local,
    /// An instance variable that `initialize` does not assign on every path.
    Initialize,
    /// A rewrite that needs syntax today's runtime does not accept yet.
    Syntax,
    /// A migration step that failed; the source was left alone.
    Internal,
}

impl Code {
    /// A stable identifier for reports.
    pub fn id(self) -> &'static str {
        match self {
            Self::Unparsed => "unparsed",
            Self::Rename => "rename",
            Self::Receiver => "receiver",
            Self::Dispatch => "dispatch",
            Self::Require => "require",
            Self::HashNew => "hash-new",
            Self::Any => "any",
            Self::Division => "division",
            Self::Condition => "condition",
            Self::Case => "case",
            Self::Local => "local",
            Self::Initialize => "initialize",
            Self::Syntax => "syntax",
            Self::Internal => "internal",
        }
    }
}

/// Migrates one source, using what `observations` recorded about it.
///
/// A source that does not compile is returned unchanged with an
/// [`Code::Unparsed`] diagnostic.
pub fn migrate(source: &str, observations: &Observations, options: &Options) -> Migration {
    migrator::migrate(source, observations.facts(source), options)
}

/// Describes migrations and their diagnostics as a JSON array, one object
/// per file, for `--report json`.
///
/// ```
/// use vibescript_tools::migrate::{Observations, Options, migrate, report_json};
/// let migration = migrate("x = [1].size()\n", &Observations::default(), &Options::default());
/// let report = report_json(&[("x.vibe", &migration)]);
/// assert!(report.contains("\"file\": \"x.vibe\""));
/// assert!(report.contains("\"changed\": true"));
/// ```
pub fn report_json(files: &[(&str, &Migration)]) -> String {
    let reports: Vec<serde_json::Value> = files
        .iter()
        .map(|(file, migration)| {
            serde_json::json!({
                "file": file,
                "changed": migration.changed,
                "diagnostics": migration.diagnostics.iter().map(|diagnostic| serde_json::json!({
                    "code": diagnostic.code.id(),
                    "line": diagnostic.line,
                    "column": diagnostic.column,
                    "message": diagnostic.message,
                })).collect::<Vec<_>>(),
            })
        })
        .collect();
    let mut text = serde_json::to_string_pretty(&reports).expect("reports serialize");
    text.push('\n');
    text
}
