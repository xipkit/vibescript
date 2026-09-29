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
    compilation::{Buffer, Table},
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

/// A scoped read, `Scope::NAME?`: where its suffix is, the namespace it is
/// read in, its scope as written, such as `Outer::Inner`, and its name.
struct Scoped {
    at: u32,
    owner: Option<u32>,
    scope: Name,
    name: Name,
}

/// The uses of suffixed bindings a lenient parse records, each at the
/// offset of the name's first `?` or `!`. A binding's id is one more than
/// the number before it, and a namespace's one more than its index.
#[derive(Default)]
pub(super) struct Uses {
    bindings: u32,
    /// Each constant binding by its namespace and name, as `3:NAME?`, with
    /// the next binding of that key, since a reopened class binds again.
    constants: Table<u32>,
    next: Buffer<u32>,
    /// Where each binding is bound, with its id.
    sites: Buffer<(u32, u32)>,
    /// Where each binding is read, with its id.
    reads: Buffer<(u32, u32)>,
    scoped: Buffer<Scoped>,
    /// Each class, module and enum by its parent's id (0 at the top level)
    /// and name, as `3:Inner`; a reopened one keeps its id.
    namespace_ids: Table<u32>,
    parents: Buffer<u32>,
    names: Buffer<Name>,
}

/// The key of `name` within the namespace `parent`, 0 at the top level.
fn key(work: &dyn Work, parent: u32, name: &str) -> Result<Name> {
    Name::join(work, &[&parent.to_string(), ":", name])
}

impl Uses {
    fn binding(
        &mut self,
        work: &dyn Work,
        name: &str,
        namespace: Option<u32>,
        at: u32,
    ) -> Result<u32> {
        self.bindings = self.bindings.saturating_add(1);
        let id = self.bindings;
        self.sites.push(work, (at, id))?;
        let mut next = 0;
        if let Some(namespace) = namespace {
            let key = key(work, namespace, name)?;
            next = self.constants.get(work, &key)?.copied().unwrap_or(0);
            self.constants.insert(work, key, id)?;
        }
        self.next.push(work, next)?;
        Ok(id)
    }

    /// The namespace named `name` in `parent`, 0 at the top level.
    fn child(&self, work: &dyn Work, parent: u32, name: &str) -> Result<Option<u32>> {
        Ok(self
            .namespace_ids
            .get(work, &key(work, parent, name)?)?
            .copied())
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
                let namespace = self.namespace.filter(|_| constant);
                uses.binding(self.work, name, namespace, suffix)
            }
        }
    }

    /// Records a binding that is not a local, such as an enum member, which
    /// scoped reads of `namespace` reach.
    pub(super) fn member_binding(
        &self,
        name: &str,
        at: usize,
        namespace: Option<u32>,
    ) -> Result<()> {
        if let (Some(namespace), Some(suffix)) = (namespace, suffix_at(self.source, name, at)) {
            let mut uses = self.suffixed.borrow_mut();
            uses.binding(self.work, name, Some(namespace), suffix)?;
        }
        Ok(())
    }

    /// In a lenient parse, the id of the class, module or enum `name`
    /// declared in the current namespace, the same when it is reopened.
    pub(super) fn namespace_entered(&self, name: &str) -> Result<Option<u32>> {
        if !lenient() {
            return Ok(None);
        }
        let parent = self.namespace.unwrap_or(0);
        let mut uses = self.suffixed.borrow_mut();
        let key = key(self.work, parent, name)?;
        if let Some(&id) = uses.namespace_ids.get(self.work, &key)? {
            return Ok(Some(id));
        }
        uses.parents.push(self.work, parent)?;
        uses.names.push(self.work, Name::new(self.work, name)?)?;
        let id = u32::try_from(uses.parents.len()).unwrap_or(u32::MAX);
        uses.namespace_ids.insert(self.work, key, id)?;
        Ok(Some(id))
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
            owner: self.namespace,
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

/// Extends the fixes in place, returning the charge for their edits. Each
/// step is a table lookup or a pass over the uses, so the work grows
/// linearly with the uses.
fn extended(
    work: &dyn Work,
    uses: &Uses,
    diagnostics: &mut [Diagnostic],
) -> Result<Option<crate::budget::Charge>> {
    // Each scoped read that resolves to a constant, with its bindings.
    let resolved = resolve(work, uses)?;
    let mut scoped = Buffer::new();
    for (read, &namespace) in uses.scoped.iter().zip(resolved.iter()) {
        work.charge(1)?;
        if namespace == 0 {
            continue;
        }
        let mut id = uses
            .constants
            .get(work, &key(work, namespace, &read.name)?)?
            .copied()
            .unwrap_or(0);
        while id != 0 {
            work.charge(1)?;
            scoped.push(work, (read.at, id))?;
            id = uses.next[id as usize - 1];
        }
    }
    // Group every use by its binding: count, then place.
    let bindings = uses.bindings as usize;
    let all = || {
        uses.sites
            .iter()
            .chain(uses.reads.iter())
            .chain(scoped.iter())
    };
    let mut starts = Buffer::with_capacity(work, bindings + 2)?;
    for _ in 0..bindings + 2 {
        starts.push(work, 0usize)?;
    }
    for &(_, id) in all() {
        work.charge(1)?;
        starts[id as usize + 1] += 1;
    }
    for id in 1..starts.len() {
        starts[id] += starts[id - 1];
    }
    let mut filled = Buffer::from_slice(work, &starts)?;
    let mut positions = Buffer::with_capacity(work, starts[bindings + 1])?;
    for _ in 0..starts[bindings + 1] {
        positions.push(work, 0u32)?;
    }
    for &(at, id) in all() {
        work.charge(1)?;
        positions[filled[id as usize]] = at;
        filled[id as usize] += 1;
    }
    // Where each binding is bound, to find a diagnostic's binding.
    let mut sites = Table::new();
    for &(at, id) in uses.sites.iter() {
        let at = Name::new(work, &at.to_string())?;
        if sites.get(work, &at)?.is_none() {
            sites.insert(work, at, id)?;
        }
    }
    let mut fixed = Buffer::with_capacity(work, bindings + 1)?;
    for _ in 0..=bindings {
        fixed.push(work, false)?;
    }
    let mut charge = None;
    for diagnostic in diagnostics.iter_mut() {
        work.charge(1)?;
        if !renames(diagnostic) {
            continue;
        }
        let edit = &diagnostic.fixes[0].edits[0];
        let at = Name::new(work, &edit.span.start.to_string())?;
        let Some(&id) = sites.get(work, &at)? else {
            continue;
        };
        let slice = &mut positions[starts[id as usize]..starts[id as usize + 1]];
        if slice.len() <= 1 {
            continue;
        }
        if fixed[id as usize] {
            diagnostic.fixes.clear();
            continue;
        }
        fixed[id as usize] = true;
        let length = slice.len();
        radix_sort(work, slice)?;
        const WHEREVER: &str = " wherever the binding is used";
        let bytes = length
            .saturating_mul(std::mem::size_of::<Edit>() + edit.replacement.len())
            .saturating_add(diagnostic.fixes[0].message.len() + WHEREVER.len());
        crate::budget::Charge::merge(&mut charge, work.reserve(bytes)?);
        // Every use spells the same name, so the same run of `?` and `!`.
        let width = edit.span.end - edit.span.start;
        let replacement = edit.replacement.clone();
        let message = format!("{}{WHEREVER}", diagnostic.fixes[0].message);
        let mut edits: Vec<Edit> = Vec::with_capacity(length);
        for &position in slice.iter() {
            work.charge(1)?;
            // A speculative parse may record a use twice.
            if edits
                .last()
                .is_some_and(|last| last.span.start == position as usize)
            {
                continue;
            }
            edits.push(Edit {
                span: Span::new(position as usize, position as usize + width),
                replacement: replacement.clone(),
            });
        }
        if edits.len() > 1 {
            diagnostic.fixes[0] = Fix::edits(message, edits);
        }
    }
    Ok(charge)
}

/// Sorts source offsets in linear time, a byte at a time.
fn radix_sort(work: &dyn Work, values: &mut [u32]) -> Result<()> {
    let mut scratch = Buffer::with_capacity(work, values.len())?;
    for &value in values.iter() {
        scratch.push(work, value)?;
    }
    for shift in [0, 8, 16, 24] {
        work.charge(values.len() + 256)?;
        let mut starts = [0usize; 257];
        for &value in values.iter() {
            starts[(value >> shift & 0xff) as usize + 1] += 1;
        }
        if starts[1..].contains(&values.len()) {
            continue;
        }
        for digit in 1..starts.len() {
            starts[digit] += starts[digit - 1];
        }
        for &value in values.iter() {
            let digit = (value >> shift & 0xff) as usize;
            scratch[starts[digit]] = value;
            starts[digit] += 1;
        }
        values.copy_from_slice(&scratch);
    }
    Ok(())
}

/// The namespace each scoped read's scope names, or 0 when the source
/// declares none, resolved as the checker resolves a capitalized name: in
/// the namespace it is read in, then at the top level, then in the nearest
/// namespace enclosing that, and from there through the rest of the path.
fn resolve(work: &dyn Work, uses: &Uses) -> Result<Buffer<u32>> {
    let mut found = Buffer::with_capacity(work, uses.scoped.len())?;
    // Reads left to the enclosing namespaces, by the one they start from.
    let mut pending = Buffer::new();
    for (index, read) in uses.scoped.iter().enumerate() {
        let head = read.scope.split("::").next().unwrap_or_default();
        let mut namespace = None;
        if let Some(owner) = read.owner {
            namespace = uses.child(work, owner, head)?;
        }
        if namespace.is_none() {
            namespace = uses.child(work, 0, head)?;
        }
        if let (None, Some(owner)) = (namespace, read.owner) {
            let parent = uses.parents[owner as usize - 1];
            if parent != 0 {
                pending.push(work, (parent, u32::try_from(index).unwrap_or(u32::MAX)))?;
            }
        }
        found.push(work, namespace.unwrap_or(0))?;
    }
    if !pending.is_empty() {
        enclosing(work, uses, &pending, &mut found)?;
    }
    for (read, namespace) in uses.scoped.iter().zip(found.iter_mut()) {
        for segment in read.scope.split("::").skip(1) {
            if *namespace == 0 {
                break;
            }
            *namespace = uses.child(work, *namespace, segment)?.unwrap_or(0);
        }
    }
    Ok(found)
}

/// Resolves each `pending` read's first scope name in the nearest of the
/// namespace it names, `(start, read)`, and those enclosing it, other than
/// the top level. One walk of the namespaces keeps, for every name, a stack
/// of the enclosing namespaces that have a member of that name, so each read
/// costs one lookup however deep it is.
fn enclosing(
    work: &dyn Work,
    uses: &Uses,
    pending: &[(u32, u32)],
    found: &mut [u32],
) -> Result<()> {
    let count = uses.parents.len();
    // Each namespace's members and pending reads, grouped by counting.
    let group = |keys: &mut dyn Iterator<Item = u32>| -> Result<(Buffer<usize>, Buffer<u32>)> {
        let mut starts = Buffer::with_capacity(work, count + 2)?;
        for _ in 0..count + 2 {
            starts.push(work, 0)?;
        }
        let mut order = Buffer::new();
        for (index, key) in keys.enumerate() {
            work.charge(1)?;
            starts[key as usize + 1] += 1;
            order.push(work, (key, u32::try_from(index).unwrap_or(u32::MAX)))?;
        }
        for index in 1..starts.len() {
            starts[index] += starts[index - 1];
        }
        let mut filled = Buffer::from_slice(work, &starts)?;
        let mut items = Buffer::with_capacity(work, order.len())?;
        for _ in 0..order.len() {
            items.push(work, 0)?;
        }
        for &(key, item) in order.iter() {
            items[filled[key as usize]] = item;
            filled[key as usize] += 1;
        }
        Ok((starts, items))
    };
    let (member_starts, members) = group(&mut uses.parents.iter().copied())?;
    let (read_starts, reads) = group(&mut pending.iter().map(|&(start, _)| start))?;
    // For each name, the innermost open namespace with a member of that
    // name: an index into `frames`, each (namespace, the frame below).
    let mut tops: Table<u32> = Table::new();
    let mut frames: Buffer<(u32, u32)> = Buffer::new();
    let mut walk: Buffer<(u32, bool)> = Buffer::new();
    for &child in &members[member_starts[0]..member_starts[1]] {
        walk.push(work, (child + 1, false))?;
    }
    while let Some((namespace, leaving)) = walk.pop() {
        work.charge(1)?;
        let own =
            &members[member_starts[namespace as usize]..member_starts[namespace as usize + 1]];
        if leaving {
            for &child in own.iter().rev() {
                let (_, below) = frames.pop().unwrap_or_default();
                tops.insert(work, uses.names[child as usize].clone(), below)?;
            }
            continue;
        }
        for &child in own {
            let name = &uses.names[child as usize];
            let below = tops.get(work, name)?.copied().unwrap_or(0);
            frames.push(work, (namespace, below))?;
            let top = u32::try_from(frames.len()).unwrap_or(u32::MAX);
            tops.insert(work, name.clone(), top)?;
        }
        for &index in &reads[read_starts[namespace as usize]..read_starts[namespace as usize + 1]] {
            let (_, read) = pending[index as usize];
            let scope = &uses.scoped[read as usize].scope;
            let head = scope.split("::").next().unwrap_or_default();
            if let Some(&top) = tops.get(work, head)?
                && top != 0
            {
                let (holder, _) = frames[top as usize - 1];
                found[read as usize] = uses.child(work, holder, head)?.unwrap_or(0);
            }
        }
        walk.push(work, (namespace, true))?;
        for &child in own {
            walk.push(work, (child + 1, false))?;
        }
    }
    Ok(())
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

    /// The review's witness: many suffixed constants in one module, and
    /// many scoped reads of a name none of them has.
    fn constants(n: usize) -> String {
        let mut source = String::from("module A\n");
        for i in 0..n {
            source.push_str(&format!("  X{i}? = {i}\n"));
        }
        source.push_str("end\n");
        source + &"p(A::NOPE?)\n".repeat(n)
    }

    /// Scoped reads, as deep as they are many, of a constant that only the
    /// outermost enclosing namespace can reach.
    fn nested(n: usize) -> String {
        let mut source = String::from("module R\nmodule A\nX? = 1\nend\n");
        source.push_str(&"module M\n".repeat(n));
        source.push_str("def self.f -> int\n");
        source.push_str(&"A::X?\n".repeat(n));
        source.push_str("end\n");
        source + &"end\n".repeat(n + 1)
    }

    /// The steps of the fix pass alone on `source`, after its parses.
    fn fix_steps(source: &str) -> u64 {
        let mut error = super::super::canonical_error_mode(source, &(), true).unwrap();
        let uses = uses(source, &()).unwrap().unwrap();
        let mut diagnostics = error.take_diagnostics();
        let mut context = CallContext::new(unlimited());
        extended(&Meter(RefCell::new(&mut context)), &uses, &mut diagnostics).unwrap();
        context.stats().steps
    }

    #[test]
    fn the_fix_pass_grows_linearly() {
        let (error, _, _) = check(&nested(4), unlimited());
        assert_eq!(error.diagnostics()[0].fixes[0].edits.len(), 5, "{error}");
        for source in [
            constants as fn(usize) -> String,
            |n| repeated(1, n),
            |n| nested(n / 10),
        ] {
            let (small, large) = (fix_steps(&source(2000)), fix_steps(&source(4000)));
            // Doubling the uses at most doubles the work.
            assert!(
                large <= small * 2,
                "{small} steps for 2,000 uses, {large} for 4,000"
            );
        }
        // Across the whole compilation too, where the parser is linear.
        for source in [constants as fn(usize) -> String, |n| repeated(1, n)] {
            let steps = |n| check(&source(n), unlimited()).1.steps;
            let (small, large) = (steps(2000), steps(4000));
            assert!(
                large * 100 <= small * 205,
                "{small} steps for 2,000 uses, {large} for 4,000"
            );
        }
    }
}
