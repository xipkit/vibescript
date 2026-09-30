//! The canonical surface of ADR-008: every removed spelling, how to find it
//! in a source, and how to rewrite it.
//!
//! The compiler reports each removed spelling as a `V04xx`
//! [`Diagnostic`](crate::diagnostic::Diagnostic) whose fix is its rewrite
//! ([`add_to`]). The rules walk a [`syntax::Tree`] that keeps every
//! construct's span, and the static checker's receiver types decide the
//! renames that depend on the receiver.
//!
//! A rewrite records its edits as one group ([`Rewrite`]), so each is offered
//! as a fix on its own. A removed spelling the rules cannot rewrite safely
//! where it stands is a [`Finding`] instead.

mod checker;
mod context;
mod edits;
mod parse;
mod patterns;
mod rules;
mod syntax;
#[cfg(test)]
mod tests;
mod walk;

#[cfg(test)]
use checker::check;
pub(crate) use checker::{add_to, entries, footprint};

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
    /// A function or method called with `::`, such as `JSON::parse(x)`.
    ScopedCall,
}

impl Rule {
    /// The diagnostic code the compiler reports the rule's spellings with.
    pub fn code(self) -> Code {
        match self {
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
            Self::ScopedCall => Code::SCOPED_CALL,
        }
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

/// A removed spelling that the walk could not rewrite where it stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Finding {
    pub rule: Rule,
    /// Where it is.
    pub span: Span,
    /// The removed spelling, for the message.
    pub removed: String,
    /// What to write instead, for the message.
    pub advice: String,
    /// Edits a person may apply after checking them, as replacement text
    /// for each span.
    pub suggestion: Vec<(Span, String)>,
}

impl Finding {
    /// A removed spelling of `rule` at `span`, with what to write instead.
    pub fn new(
        rule: Rule,
        span: Span,
        removed: impl Into<String>,
        advice: impl Into<String>,
    ) -> Self {
        Self {
            rule,
            span,
            removed: removed.into(),
            advice: advice.into(),
            suggestion: Vec::new(),
        }
    }
}
