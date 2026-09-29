//! Where V0003's rename of a suffixed binding reaches. A binding such as
//! `ok? = true` or `def f(list!: array<int>)` is reported at its suffix, and
//! the fix renames the binding everywhere it is used: each place that binds
//! it and each read of it in scope. A read counts only where the parser's
//! scope says the name is that binding, so a suffixed method that merely
//! shares the name, such as a host's `ready?`, is never renamed.
//!
//! The uses come from a second, lenient parse that accepts suffixed
//! bindings, as the grammar did before ADR-008, and tracks each binding
//! through the parser's own scopes.

use super::{Error, Parser, Result, Work, name_suffix_position};
use crate::{
    compilation::Buffer,
    diagnostic::{Code, Edit, Fix, Span},
};
use std::cell::Cell;

thread_local! {
    /// Whether the parse in progress accepts suffixed bindings and records
    /// their uses.
    static LENIENT: Cell<bool> = const { Cell::new(false) };
}

/// Whether the parse in progress is a lenient one.
pub(super) fn lenient() -> bool {
    LENIENT.with(Cell::get)
}

/// The uses of suffixed bindings a lenient parse records, each at the
/// offset of the name's first `?` or `!`.
#[derive(Default)]
pub(super) struct Uses {
    /// How many bindings there are; each has its index plus one as its id.
    bindings: u32,
    /// Where each binding is bound, with its id.
    sites: Buffer<(u32, u32)>,
    /// Where each binding is read, with its id.
    reads: Buffer<(u32, u32)>,
}

impl Uses {
    fn binding(&mut self, work: &dyn Work, at: u32) -> Result<u32> {
        self.bindings = self.bindings.saturating_add(1);
        self.sites.push(work, (at, self.bindings))?;
        Ok(self.bindings)
    }
}

/// The offset of the `?` or `!` that ends `name`, spelled at `at`, perhaps
/// after a sigil, or none when the name has no suffix or one inside it.
fn suffix_at(source: &str, name: &str, at: usize) -> Option<u32> {
    let suffix = name_suffix_position(name)?;
    if !name[suffix..].bytes().all(|b| matches!(b, b'?' | b'!')) {
        return None;
    }
    let sigil = source[at..].len() - source[at..].trim_start_matches('@').len();
    u32::try_from(at + sigil + suffix).ok()
}

impl Parser<'_> {
    /// The id to record with `name` in the locals, as it is bound at `at`:
    /// the suffixed binding already in scope, a new one, or 0.
    pub(super) fn local_id(&self, name: &str, at: usize) -> Result<u32> {
        if !lenient() {
            return Ok(0);
        }
        let Some(suffix) = suffix_at(self.source, name, at) else {
            return Ok(0);
        };
        let mut uses = self.suffixed.borrow_mut();
        match self.locals.get(self.work, name)? {
            Some(&id) if id != 0 => {
                uses.sites.push(self.work, (suffix, id))?;
                Ok(id)
            }
            _ => uses.binding(self.work, suffix),
        }
    }

    /// Records the bare read of `name`, just consumed, when it is a
    /// suffixed binding in scope.
    pub(super) fn suffix_read(&self, name: &str) -> Result<()> {
        if !lenient() {
            return Ok(());
        }
        let at = self.tokens[self.pos - 1].offset;
        let Some(suffix) = suffix_at(self.source, name, at) else {
            return Ok(());
        };
        if let Some(&id) = self.locals.get(self.work, name)?
            && id != 0
        {
            self.suffixed
                .borrow_mut()
                .reads
                .push(self.work, (suffix, id))?;
        }
        Ok(())
    }
}

/// Parses `source` accepting suffixed bindings, returning their uses, or
/// none when it does not parse that way either.
fn uses(source: &str, work: &dyn Work) -> Result<Option<Uses>> {
    struct Restore(bool, bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            LENIENT.with(|lenient| lenient.set(self.0));
            super::CANONICAL.with(|canonical| canonical.set(self.1));
        }
    }
    let _restore = Restore(
        LENIENT.with(|lenient| lenient.replace(true)),
        super::CANONICAL.with(|canonical| canonical.replace(false)),
    );
    let parser = match super::parser(source, work) {
        Ok(parser) => parser,
        Err(error) if error.kind == crate::ErrorKind::Syntax => return Ok(None),
        Err(error) => return Err(error),
    };
    let parsing = super::Parsing::<super::recovery::FailFast>::new(parser);
    match parsing.run(super::Call::Program) {
        Ok(_) => Ok(Some(parsing.parser.into_inner().suffixed.into_inner())),
        Err(error) if error.kind == crate::ErrorKind::Syntax => Ok(None),
        Err(error) => Err(error),
    }
}

/// Extends each V0003 fix of `error` that renames a suffixed binding with
/// the same edit at the binding's other sites and reads, so one fix renames
/// the binding whole.
pub(super) fn extend_fixes(source: &str, work: &dyn Work, error: Error) -> Error {
    let renames = |diagnostic: &crate::diagnostic::Diagnostic| {
        diagnostic.code == Code::NAME_SUFFIX
            && diagnostic
                .applicable_fix()
                .is_some_and(|fix| fix.edits.len() == 1)
    };
    if !error.diagnostics().iter().any(renames) {
        return error;
    }
    let uses = match uses(source, work) {
        Ok(Some(uses)) => uses,
        Ok(None) => return error,
        Err(failure) => return failure,
    };
    match extended(work, &uses, error.diagnostics(), renames) {
        Ok(diagnostics) => error.with_diagnostics(diagnostics),
        Err(failure) => failure,
    }
}

fn extended(
    work: &dyn Work,
    uses: &Uses,
    diagnostics: &[crate::diagnostic::Diagnostic],
    renames: impl Fn(&crate::diagnostic::Diagnostic) -> bool,
) -> Result<Vec<crate::diagnostic::Diagnostic>> {
    work.charge(uses.sites.len() + uses.reads.len())?;
    let mut out = Vec::with_capacity(diagnostics.len());
    for diagnostic in diagnostics {
        let mut diagnostic = diagnostic.clone();
        if !renames(&diagnostic) {
            out.push(diagnostic);
            continue;
        }
        let edit = diagnostic.fixes[0].edits[0].clone();
        let Ok(at) = u32::try_from(edit.span.start) else {
            out.push(diagnostic);
            continue;
        };
        let Some(&(_, id)) = uses.sites.iter().find(|(site, _)| *site == at) else {
            out.push(diagnostic);
            continue;
        };
        let mut positions: Vec<u32> = uses
            .sites
            .iter()
            .chain(uses.reads.iter())
            .filter(|(_, other)| *other == id)
            .map(|(position, _)| *position)
            .collect();
        work.charge(positions.len())?;
        positions.sort_unstable();
        positions.dedup();
        if positions.len() > 1 {
            let edits = positions
                .into_iter()
                .map(|position| Edit {
                    span: Span::new(position as usize, position as usize + 1),
                    replacement: edit.replacement.clone(),
                })
                .collect();
            let message = format!(
                "{} wherever the binding is used",
                diagnostic.fixes[0].message
            );
            diagnostic.fixes[0] = Fix::edits(message, edits);
        }
        out.push(diagnostic);
    }
    Ok(out)
}
