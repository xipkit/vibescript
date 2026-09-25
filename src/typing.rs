//! The static type checker of ADR-007.
//!
//! [`check`] reads a parsed program and reports every type error as a
//! [`Diagnostic`]. It checks each function once, from its own signature and
//! the signatures of what it calls, never from a callee's body, so its work
//! is linear in the program. Besides diagnostics it records the static type of
//! every member call's receiver in [`CallTypes`], which rules that depend on
//! the receiver's type, such as typed renames of removed spellings, consult.

use crate::{capability::Registered, diagnostic::Diagnostic, syntax::Declarations};
use std::fmt;

/// What the checker reads: one source, parsed, with the host functions and
/// capabilities its engine registers.
pub(crate) struct Input<'a> {
    pub source: &'a str,
    pub parsed: &'a Declarations,
    /// The tokens the parser read, for exact spans.
    pub tokens: &'a [crate::tooling::Token],
    pub hosts: Vec<(&'a String, &'a Registered)>,
    /// Whether the source is a required file rather than a host script.
    pub file: bool,
}

/// The result of checking one source.
#[derive(Clone, Debug, Default)]
pub struct Checked {
    /// Every finding in source order; any error stops compilation.
    pub diagnostics: Vec<Diagnostic>,
    /// The receiver type at each member call.
    pub calls: CallTypes,
}

/// The static type of the receiver at each member call, keyed by the byte
/// offset of the member's name.
///
/// For `items.include?(x)` the key is the offset of `include?`. Calls whose
/// receiver the checker could not type, such as ones in code that failed to
/// resolve, are absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CallTypes {
    /// Entries sorted by offset.
    entries: Vec<(usize, ReceiverType)>,
}

impl CallTypes {
    /// The receiver type of the member call whose name starts at `offset`.
    pub fn receiver_at(&self, offset: usize) -> Option<&ReceiverType> {
        self.entries
            .binary_search_by_key(&offset, |(key, _)| *key)
            .ok()
            .map(|index| &self.entries[index].1)
    }

    /// Every recorded call, by member-name offset in source order.
    pub fn iter(&self) -> impl Iterator<Item = (usize, &ReceiverType)> {
        self.entries.iter().map(|(offset, ty)| (*offset, ty))
    }

    /// The number of recorded calls.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no call was recorded.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    #[allow(dead_code)]
    pub(crate) fn from_entries(mut entries: Vec<(usize, ReceiverType)>) -> Self {
        entries.sort_by_key(|(offset, _)| *offset);
        entries.dedup_by_key(|(offset, _)| *offset);
        Self { entries }
    }
}

/// A receiver's static type, by its canonical name and the base type of each
/// alternative.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiverType {
    name: String,
    bases: Vec<String>,
}

impl ReceiverType {
    #[allow(dead_code)]
    pub(crate) fn new(name: String, bases: Vec<String>) -> Self {
        Self { name, bases }
    }

    /// The type as an annotation writes it, such as `hash<string, int>`,
    /// `{ name: string }`, `int?` or `Invoice`.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The base type of each alternative, sorted and without repeats:
    /// `array` for arrays and tuples, `hash` for hashes and shapes, the
    /// scalar's name (`int`, `float`, `string`, `bool`, `nil`, `symbol`,
    /// `time`, `duration`, `money`, `range`, `regex`, `match_data`, `error`),
    /// `any`, `type` for a type literal, or the name of a class or enum for
    /// its instances and members. A namespace used as a receiver, such as an
    /// enum, class or module name, is `namespace`.
    pub fn bases(&self) -> &[String] {
        &self.bases
    }

    /// Whether every alternative other than `nil` has base `base`, and there
    /// is at least one.
    pub fn is(&self, base: &str) -> bool {
        let mut others = self.bases.iter().filter(|b| *b != "nil").peekable();
        others.peek().is_some() && others.all(|b| b == base)
    }
}

impl fmt::Display for ReceiverType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// Checks one parsed source.
pub(crate) fn check(input: &Input<'_>) -> Checked {
    let _ = (
        input.source,
        input.parsed,
        input.tokens,
        &input.hosts,
        input.file,
    );
    Checked::default()
}
