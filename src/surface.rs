//! The canonical surface of ADR-008: every removed spelling, how to find it
//! in a source, and how to rewrite it.
//!
//! One set of rules serves two callers. The compiler reports each removed
//! spelling as a `V04xx` [`Diagnostic`](crate::diagnostic::Diagnostic) when
//! static types are on ([`check`]), and `vibes migrate` applies the same
//! rewrites to old-language sources, deciding with the types it observed
//! when the static ones are unknown. Both walk a [`syntax::Tree`] that keeps
//! every construct's span, through the [`Walk`] trait: its provided methods
//! traverse the tree and apply each rule, and the [`Hooks`] an implementor
//! supplies say what it knows about the program's types and add rules of
//! its own.
//!
//! A rewrite records its edits as one group ([`Rewrite`]), so the compiler
//! can offer it as a fix on its own while the migration renders every
//! group at once, nested edits inside one another. A removed spelling the
//! rules cannot rewrite safely where it stands is a [`Finding`] instead.
//!
//! ```
//! let diagnostics = vibescript::surface::check("items = [1]\nn = items.size\n")?;
//! assert_eq!(diagnostics[0].code.to_string(), "V0401");
//! assert_eq!(diagnostics[0].message, "`size` was removed; use `length`");
//! let fixed = diagnostics[0].fixes[0].apply("items = [1]\nn = items.size\n");
//! assert_eq!(fixed.as_deref(), Some("items = [1]\nn = items.length\n"));
//! # Ok::<(), vibescript::Error>(())
//! ```

mod checker;
mod context;
pub mod edits;
mod hooks;
pub mod parse;
pub mod patterns;
mod probe;
mod rules;
pub mod syntax;
#[cfg(test)]
mod tests;
mod walk;

pub(crate) use checker::add_to;
pub use checker::{check, check_tokens};
pub use context::{
    Declared, Place, Scope, Surface, collect_expr, collect_locals, collect_rescued, literal_type,
    method_name, namespace_member_takes_no_arguments, namespace_name, primary, simple,
    string_literal, symbol_literal,
};
pub use hooks::{Annotation, Hooks, Probe, Test};
pub use probe::{member_without_parens, namespace_without_parens};
pub use rules::{Captures, Rules};
pub use walk::Walk;

use crate::diagnostic::Code;
use syntax::Span;

/// Which removed spelling a rewrite or finding concerns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Rule {
    /// A member, function, namespace member or keyword from the rename table.
    Name,
    /// `x.nil?`.
    NilPredicate,
    /// `eql?` and `equal?`.
    Equality,
    /// `itself`, `tap` and `yield_self`.
    Identity,
    /// `send`, `public_send` and `respond_to?`.
    Dispatch,
    /// A `do ... end` block.
    DoBlock,
    /// `unless`, as a statement or a modifier.
    Unless,
    /// `until`, as a statement or a modifier.
    Until,
    /// `h[:name]`.
    SymbolKey,
    /// `%w[...]` and `%i[...]`.
    PercentLiteral,
    /// `Hash.new`.
    HashNew,
    /// `()` on a call without arguments.
    EmptyParentheses,
    /// A type name in another case than lowercase, or `object`.
    TypeName,
    /// A symbol naming a required module or its alias.
    Require,
    /// A keyword parameter declared as `name:`, `name: default` or
    /// `name: T:`, rather than after a bare `*`.
    KeywordParameter,
    /// A hash field read or written with a dot, `h.name`.
    FieldAccess,
}

impl Rule {
    /// The diagnostic code the compiler reports the rule's spellings with.
    pub fn code(self) -> Option<Code> {
        Some(match self {
            Self::Name => Code::REMOVED_NAME,
            Self::NilPredicate => Code::NIL_PREDICATE,
            Self::Equality => Code::IDENTITY_EQUALITY,
            Self::Identity => Code::IDENTITY_CALL,
            Self::Dispatch => Code::DISPATCH_BY_NAME,
            Self::DoBlock => Code::DO_BLOCK,
            Self::Unless => Code::UNLESS,
            Self::Until => Code::UNTIL,
            Self::SymbolKey => Code::SYMBOL_KEY,
            Self::PercentLiteral => Code::PERCENT_LITERAL,
            Self::HashNew => Code::HASH_NEW,
            Self::EmptyParentheses => Code::EMPTY_PARENTHESES,
            Self::TypeName => Code::TYPE_NAME,
            Self::Require => Code::DYNAMIC_REQUIRE,
            Self::KeywordParameter => Code::KEYWORD_PARAMETER,
            Self::FieldAccess => Code::FIELD_ACCESS,
        })
    }
}

/// How a dot reaches a hash field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// `h.name` as a value.
    Read,
    /// `h.name = value`, the only target of an assignment.
    Write,
    /// `h.name += value` and the other operators that read the field first.
    Update,
    /// `h.name, other = ...` or a loop variable, which an index cannot be.
    Destructure,
}

/// Why a migration leaves a finding to a person.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum Reason {
    /// A removed spelling whose replacement needs a person.
    Rename,
    /// A removed spelling whose receiver type is unknown or mixed.
    Receiver,
    /// Dispatch by a name known only at runtime.
    Dispatch,
    /// A `require` whose path or alias is not a string literal.
    Require,
    /// `Hash.new` with a default or a block.
    HashNew,
    /// A condition on a value that is not always a `bool`.
    Condition,
    /// A rewrite that needs syntax the target does not accept.
    Syntax,
}

/// A removed spelling that the walk rewrote. Its edits are the group of
/// the same index in [`edits::Edits`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rewrite {
    pub rule: Rule,
    /// Where a diagnostic about it points.
    pub span: Span,
    /// The removed spelling, such as `size`.
    pub removed: String,
    /// What replaces it, such as "use `length`".
    pub advice: String,
}

/// A removed spelling that the walk could not rewrite where it stands, or
/// something else a migration must leave to a person.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    /// The removed spelling's rule, or none for a finding outside the
    /// canonical surface, such as a condition on a value that is not a `bool`.
    pub rule: Option<Rule>,
    /// Why a migration reports it, or none when only the compiler does.
    pub reason: Option<Reason>,
    /// Where it is; a migration reports the start.
    pub span: Span,
    /// What a migration report says.
    pub message: String,
    /// The removed spelling, for the compiler's message.
    pub removed: String,
    /// What to write instead, for the compiler's message.
    pub advice: String,
    /// Edits a person may apply after checking them, as replacement text
    /// for each span.
    pub suggestion: Vec<(Span, String)>,
}

impl Finding {
    /// A finding that a migration reports for `reason`, outside the
    /// canonical surface.
    pub fn new(reason: Reason, span: Span, message: impl Into<String>) -> Self {
        Self {
            rule: None,
            reason: Some(reason),
            span,
            message: message.into(),
            removed: String::new(),
            advice: String::new(),
            suggestion: Vec::new(),
        }
    }

    /// A removed spelling that only the compiler reports, since a migration
    /// has a reason of its own to leave it.
    pub fn removed(
        rule: Rule,
        span: Span,
        removed: impl Into<String>,
        advice: impl Into<String>,
    ) -> Self {
        Self {
            rule: Some(rule),
            reason: None,
            span,
            message: String::new(),
            removed: removed.into(),
            advice: advice.into(),
            suggestion: Vec::new(),
        }
    }

    /// Makes a finding also concern the removed spelling `removed` of `rule`.
    pub fn spelling(
        mut self,
        rule: Rule,
        removed: impl Into<String>,
        advice: impl Into<String>,
    ) -> Self {
        self.rule = Some(rule);
        self.removed = removed.into();
        self.advice = advice.into();
        self
    }
}
