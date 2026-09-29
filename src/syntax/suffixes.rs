//! Where V0003's rename of a suffixed binding reaches. A binding such as
//! `ok? = true` or `def f(list!: array<int>)` is reported at its suffix, and
//! the fix renames the binding everywhere it is used: each place that binds
//! it and each read of it in scope, including scoped reads of a namespace's
//! constant, such as `Limits::MAX!`. A read counts only where the parser's
//! scope says the name is that binding, and a scoped read only where its
//! scope resolves to the constant's namespace as the checker resolves it, so
//! a suffixed method that merely shares the name, such as a host's `ready?`,
//! is never renamed.
//!
//! The uses come from a second, lenient parse that accepts suffixed
//! bindings, as the grammar did before ADR-008, and tracks each binding
//! through the parser's own scopes. Every table and every edit of a fix is
//! charged to the caller's work.

use super::{Error, Expr, Name, Node, Parser, Result, Work, name_suffix_position};
use crate::{
    compilation::Buffer,
    diagnostic::{Code, Diagnostic, Edit, Fix, Span},
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

/// A suffixed binding: its name, and the path of the namespace whose body
/// declares it, which a scoped read resolves to.
struct Binding {
    name: Name,
    namespace: Option<Name>,
}

/// A scoped read, `Scope::NAME?`: where its suffix is, the namespace path
/// it is read in, its scope as written and its name.
struct Scoped {
    at: u32,
    owner: Option<Name>,
    scope: Name,
    name: Name,
}

/// The uses of suffixed bindings a lenient parse records, each at the
/// offset of the name's first `?` or `!`.
#[derive(Default)]
pub(super) struct Uses {
    bindings: Buffer<Binding>,
    /// Where each binding is bound, with its id: its index plus one.
    sites: Buffer<(u32, u32)>,
    /// Where each binding is read, with its id.
    reads: Buffer<(u32, u32)>,
    scoped: Buffer<Scoped>,
    /// The path of every class, module and enum the source declares.
    namespaces: Buffer<Name>,
}

impl Uses {
    fn binding(
        &mut self,
        work: &dyn Work,
        name: &str,
        namespace: Option<&Name>,
        at: u32,
    ) -> Result<u32> {
        self.bindings.push(
            work,
            Binding {
                name: Name::new(work, name)?,
                namespace: namespace.cloned(),
            },
        )?;
        let id = u32::try_from(self.bindings.len()).unwrap_or(u32::MAX);
        self.sites.push(work, (at, id))?;
        Ok(id)
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
            _ => {
                // Only a capitalized name bound in a namespace's own body is
                // a constant that scoped reads reach.
                let constant = self.namespace_body && name.starts_with(super::unicode::upper);
                let namespace = self.namespace.as_ref().filter(|_| constant);
                uses.binding(self.work, name, namespace, suffix)
            }
        }
    }

    /// Records a binding that is not a local, such as an enum member, which
    /// scoped reads of `namespace` reach.
    pub(super) fn member_binding(&self, name: &str, at: usize, namespace: &Name) -> Result<()> {
        if let (true, Some(suffix)) = (lenient(), suffix_at(self.source, name, at)) {
            let mut uses = self.suffixed.borrow_mut();
            uses.binding(self.work, name, Some(namespace), suffix)?;
        }
        Ok(())
    }

    /// Records that the source declares the class, module or enum `path`.
    pub(super) fn namespace_declared(&self, path: &Name) -> Result<()> {
        if lenient() {
            let mut uses = self.suffixed.borrow_mut();
            uses.namespaces.push(self.work, path.clone())?;
        }
        Ok(())
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

    /// Records the scoped read of `name`, just consumed, through `scope`
    /// when that is a path of names, such as `Outer::Inner`.
    pub(super) fn scoped_suffix_read(&self, scope: &Expr, name: &str) -> Result<()> {
        if !lenient() {
            return Ok(());
        }
        let at = self.tokens[self.pos - 1].offset;
        let Some(suffix) = suffix_at(self.source, name, at) else {
            return Ok(());
        };
        let Some(scope) = self.scope_path(scope)? else {
            return Ok(());
        };
        let entry = Scoped {
            at: suffix,
            owner: self.namespace.clone(),
            scope,
            name: Name::new(self.work, name)?,
        };
        self.suffixed.borrow_mut().scoped.push(self.work, entry)?;
        Ok(())
    }

    /// The path `expr` names, such as `Outer::Inner`, or none.
    fn scope_path(&self, expr: &Expr) -> Result<Option<Name>> {
        let mut names: Buffer<&str> = Buffer::new();
        let mut expr = expr;
        loop {
            self.work.charge(1)?;
            match &expr.node {
                Node::Var(name) => {
                    names.push(self.work, name)?;
                    break;
                }
                Node::Scope(inner, name, None) => {
                    names.push(self.work, name)?;
                    expr = inner;
                }
                _ => return Ok(None),
            }
        }
        let mut pieces: Buffer<&str> = Buffer::new();
        for (index, name) in names.iter().rev().enumerate() {
            if index > 0 {
                pieces.push(self.work, "::")?;
            }
            pieces.push(self.work, name)?;
        }
        Name::join(self.work, &pieces).map(Some)
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

/// Whether a V0003 diagnostic's fix removes or replaces one suffix, which
/// may rename a binding.
fn renames(diagnostic: &Diagnostic) -> bool {
    diagnostic.code == Code::NAME_SUFFIX
        && diagnostic
            .applicable_fix()
            .is_some_and(|fix| fix.edits.len() == 1)
}

/// Gives the first V0003 fix of each suffixed binding in `error` the same
/// edit at every other place the binding is bound or read, so one fix
/// renames the binding whole. The binding's other V0003 diagnostics keep
/// no fix of their own, which could otherwise rename only part of it.
pub(super) fn extend_fixes(source: &str, work: &dyn Work, mut error: Error) -> Error {
    if !error.diagnostics().iter().any(renames) {
        return error;
    }
    let uses = match uses(source, work) {
        Ok(Some(uses)) => uses,
        Ok(None) => return error,
        Err(failure) => return failure,
    };
    let mut diagnostics = error.take_diagnostics();
    let charge = match extended(work, &uses, &mut diagnostics) {
        Ok(charge) => charge,
        Err(failure) => return failure,
    };
    let mut error = error.with_diagnostics(diagnostics);
    match error.retain(work, charge) {
        Ok(()) => error,
        Err(failure) => failure,
    }
}

/// Extends the fixes in place, returning the charge for their edits.
fn extended(
    work: &dyn Work,
    uses: &Uses,
    diagnostics: &mut [Diagnostic],
) -> Result<Option<crate::budget::Charge>> {
    // Each binding's positions, grouped by its id.
    let mut positions = Buffer::new();
    for &(at, id) in uses.sites.iter().chain(uses.reads.iter()) {
        positions.push(work, (id, at))?;
    }
    let mut namespaces = Buffer::new();
    for namespace in uses.namespaces.iter() {
        namespaces.push(work, namespace.clone())?;
    }
    sort(work, &mut namespaces)?;
    for scoped in uses.scoped.iter() {
        let Some(path) = resolve(work, &namespaces, scoped)? else {
            continue;
        };
        for (index, binding) in uses.bindings.iter().enumerate() {
            work.charge(1)?;
            if binding.namespace.as_ref() == Some(&path) && binding.name == scoped.name {
                let id = u32::try_from(index + 1).unwrap_or(u32::MAX);
                positions.push(work, (id, scoped.at))?;
            }
        }
    }
    sort(work, &mut positions)?;
    positions.dedup();
    // Each binding's sites by where they are.
    let mut sites = Buffer::new();
    for &site in uses.sites.iter() {
        sites.push(work, site)?;
    }
    sort(work, &mut sites)?;
    let mut fixed = Buffer::new();
    let mut charge = None;
    for diagnostic in diagnostics.iter_mut() {
        work.charge(1)?;
        if !renames(diagnostic) {
            continue;
        }
        let edit = &diagnostic.fixes[0].edits[0];
        let Ok(at) = u32::try_from(edit.span.start) else {
            continue;
        };
        let Ok(site) = sites.binary_search_by_key(&at, |&(site, _)| site) else {
            continue;
        };
        let id = sites[site].1;
        let start = positions.partition_point(|&(other, _)| other < id);
        let end = positions.partition_point(|&(other, _)| other <= id);
        if end - start <= 1 {
            continue;
        }
        work.charge(fixed.len())?;
        if fixed.contains(&id) {
            diagnostic.fixes.clear();
            continue;
        }
        fixed.push(work, id)?;
        const WHEREVER: &str = " wherever the binding is used";
        let count = end - start;
        let bytes = count
            .saturating_mul(std::mem::size_of::<Edit>() + edit.replacement.len())
            .saturating_add(diagnostic.fixes[0].message.len() + WHEREVER.len());
        crate::budget::Charge::merge(&mut charge, work.reserve(bytes)?);
        work.charge(count)?;
        let replacement = edit.replacement.clone();
        let message = format!("{}{WHEREVER}", diagnostic.fixes[0].message);
        let mut edits = Vec::with_capacity(count);
        for &(_, position) in &positions[start..end] {
            edits.push(Edit {
                span: Span::new(position as usize, position as usize + 1),
                replacement: replacement.clone(),
            });
        }
        diagnostic.fixes[0] = Fix::edits(message, edits);
    }
    Ok(charge)
}

/// Sorts `values`, charging for the comparisons.
fn sort<T: Ord>(work: &dyn Work, values: &mut Buffer<T>) -> Result<()> {
    let length = values.len();
    let log = usize::BITS - length.leading_zeros();
    work.charge(length.saturating_mul(log as usize))?;
    values.sort_unstable();
    Ok(())
}

/// The namespace `scoped` names, resolved as the checker resolves a
/// capitalized name: in the namespace it is read in, then at the top
/// level, then in each namespace enclosing that, and from there through
/// the rest of the path. None when the source declares no such namespace.
fn resolve(work: &dyn Work, namespaces: &[Name], scoped: &Scoped) -> Result<Option<Name>> {
    let declared = |path: &str| -> Result<bool> {
        work.charge(1)?;
        Ok(namespaces
            .binary_search_by(|name| name.as_str().cmp(path))
            .is_ok())
    };
    let joined = |outer: &str, inner: &str| Name::join(work, &[outer, "::", inner]);
    let (head, rest) = match scoped.scope.split_once("::") {
        Some((head, rest)) => (head, Some(rest)),
        None => (scoped.scope.as_str(), None),
    };
    let mut base = None;
    if let Some(owner) = &scoped.owner {
        let candidate = joined(owner, head)?;
        if declared(&candidate)? {
            base = Some(candidate);
        }
    }
    if base.is_none() && declared(head)? {
        base = Some(Name::new(work, head)?);
    }
    let mut enclosing = scoped.owner.as_ref().map(Name::as_str);
    while base.is_none() {
        let Some((outer, _)) = enclosing.and_then(|path| path.rsplit_once("::")) else {
            break;
        };
        let candidate = joined(outer, head)?;
        if declared(&candidate)? {
            base = Some(candidate);
        }
        enclosing = Some(outer);
    }
    let Some(base) = base else {
        return Ok(None);
    };
    let path = match rest {
        Some(rest) => joined(&base, rest)?,
        None => base,
    };
    Ok(declared(&path)?.then_some(path))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallContext, CallOptions, ErrorKind, compilation::Meter};
    use std::cell::RefCell;

    /// One binding that recovery reports at each of its `sites` and that is
    /// read `reads` times, so its fix holds an edit for every use.
    fn repeated(sites: usize, reads: usize) -> String {
        "x? = 1\n".repeat(sites) + &"p(x?)\n".repeat(reads)
    }

    /// Checks `source` as a host compilation does, under `options`.
    fn check(source: &str, options: CallOptions) -> (Error, crate::Stats, usize) {
        let mut context = CallContext::new(options);
        let error = {
            let work = Meter(RefCell::new(&mut context));
            match super::super::parse(source, &work) {
                Err(error) => super::super::host_syntax(source, &work, error),
                Ok(_) => panic!("{source} parses"),
            }
        };
        let stats = context.stats();
        let fixes = error
            .diagnostics()
            .iter()
            .filter(|d| d.applicable_fix().is_some())
            .count();
        (error, stats, fixes)
    }

    fn unlimited() -> CallOptions {
        let mut options = CallOptions::default();
        options.limits.steps = None;
        options.limits.memory_bytes = None;
        options
    }

    #[test]
    fn a_binding_fix_is_built_once_and_charged() {
        let (few, few_stats, _) = check(&repeated(100, 1), unlimited());
        let (error, stats, fixes) = check(&repeated(100, 4000), unlimited());
        assert_eq!(error.kind, ErrorKind::Syntax, "{error}");
        // Recovery reports every site, but only the first carries the fix.
        let diagnostics = error.diagnostics();
        assert!(diagnostics.len() > 50, "{}", diagnostics.len());
        assert_eq!(fixes, 1);
        assert_eq!(diagnostics[0].fixes[0].edits.len(), 4100);
        // The edits are held as long as the error, beyond what one read costs.
        assert!(
            stats.retained_memory_bytes
                >= few_stats.retained_memory_bytes + 3999 * std::mem::size_of::<Edit>(),
            "{stats:?} {few_stats:?}"
        );
        assert!(stats.steps > few_stats.steps + 3999);
        drop((few, error));
        for memory in [stats.peak_memory_bytes - 1, stats.peak_memory_bytes * 3 / 4] {
            let mut options = unlimited();
            options.limits.memory_bytes = Some(memory);
            let (error, _, _) = check(&repeated(100, 4000), options);
            assert_eq!(error.kind, ErrorKind::Memory, "{error}");
        }
        for steps in [stats.steps - 1, stats.steps * 3 / 4] {
            let mut options = unlimited();
            options.limits.steps = Some(steps);
            let (error, _, _) = check(&repeated(100, 4000), options);
            assert_eq!(error.kind, ErrorKind::Steps, "{error}");
        }
    }
}
