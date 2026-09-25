//! The static type checker of ADR-007.
//!
//! [`check`] reads a parsed program and reports every type error as a
//! [`Diagnostic`]. It checks each function once, from its own signature and
//! the signatures of what it calls, never from a callee's body, so its work
//! is linear in the program. Besides diagnostics it records the static type of
//! every member call's receiver in [`CallTypes`], which rules that depend on
//! the receiver's type, such as typed renames of removed spellings, consult.

use crate::{capability::Registered, diagnostic::Diagnostic, syntax::Declarations};
use std::{collections::HashMap, fmt};

mod calls;
mod check;
mod expr;
mod flow;
mod program;
mod sigs;
mod spans;
mod ty;

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
    /// A deterministic count of the checker's work, which grows linearly
    /// with the program; compilation charges it to the step quota.
    pub steps: u64,
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
    let mut checker = Checker {
        source: input.source,
        parsed: input.parsed,
        spans: spans::Spans::new(input.source, input.tokens),
        types: ty::Types::new(),
        program: program::Program::default(),
        converter: sigs::Converter::default(),
        diagnostics: Vec::new(),
        calls: Vec::new(),
        constants: HashMap::new(),
        frame: check::Frame::new(None, false, None, String::new()),
        purposes: Vec::new(),
        mute: 0,
        steps: 0,
    };
    let _ = input.file;
    for (name, host) in &input.hosts {
        let function = crate::signatures::host::function(name, host);
        let sig = checker
            .converter
            .convert_owned(&mut checker.types, &function, None);
        checker
            .program
            .hosts
            .insert((*name).clone(), std::rc::Rc::new(sig));
    }
    checker.declare_program(input.parsed);
    checker.check_all();
    let mut diagnostics = checker.diagnostics;
    diagnostics.sort_by_key(|d| (d.span.start, d.span.end));
    diagnostics.dedup_by(|a, b| a.code == b.code && a.span == b.span && a.message == b.message);
    let steps =
        checker.steps + checker.frame.flow.steps + checker.types.steps + checker.spans.steps.get();
    Checked {
        diagnostics,
        calls: CallTypes::from_entries(checker.calls),
        steps,
    }
}

/// Checks that the command line can call `function` with `count` arguments,
/// which it passes as strings: each positional parameter they bind must
/// accept `string`, and a rest parameter `array<string>`.
pub(crate) fn entry_arguments(input: &Input<'_>, function: &str, count: usize) -> Vec<Diagnostic> {
    let mut checker = Checker {
        source: input.source,
        parsed: input.parsed,
        spans: spans::Spans::new(input.source, input.tokens),
        types: ty::Types::new(),
        program: program::Program::default(),
        converter: sigs::Converter::default(),
        diagnostics: Vec::new(),
        calls: Vec::new(),
        constants: HashMap::new(),
        frame: check::Frame::new(None, false, None, String::new()),
        purposes: Vec::new(),
        mute: 0,
        steps: 0,
    };
    checker.declare_program(input.parsed);
    checker.diagnostics.clear();
    checker.entry_arguments(function, count);
    checker.diagnostics
}

/// The state of one check.
pub(crate) struct Checker<'a> {
    source: &'a str,
    parsed: &'a Declarations,
    spans: spans::Spans<'a>,
    types: ty::Types,
    program: program::Program<'a>,
    converter: sigs::Converter,
    diagnostics: Vec<Diagnostic>,
    calls: Vec<(usize, ReceiverType)>,
    /// Constants of class and module bodies, by namespace and name.
    constants: HashMap<(Option<program::NsId>, String), ty::Ty>,
    frame: check::Frame,
    /// Why the value being checked against a type is checked, innermost last.
    purposes: Vec<check::Purpose>,
    /// While positive, diagnostics are dropped: a second look at code that
    /// was already checked.
    mute: u32,
    /// Work done outside the types, spans and flow of the current function.
    steps: u64,
}

/// The name of a symbol literal's value.
fn symbol_text(value: &crate::Value) -> Option<String> {
    match &value.0 {
        crate::value::Kind::Symbol(symbol) => {
            Some(String::from_utf8_lossy(&symbol.data).into_owned())
        }
        _ => None,
    }
}
