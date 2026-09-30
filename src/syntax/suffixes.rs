//! Where V0003's rename of a suffixed binding reaches. A binding such as
//! `ok? = true` or `def f(list!: array<int>)` is reported at its suffix, and
//! the fix renames the binding everywhere it is used: each place that binds
//! it and each read of it in scope, including scoped reads of a namespace's
//! constant, such as `Limits::MAX!`. A read counts only where the parser's
//! scope says the name is that binding, and a scoped read only where its
//! scope resolves to the constant's namespace as the checker resolves it, so
//! a suffixed method that merely shares the name, such as a host's `ready?`,
//! is never renamed. A class, module, enum or type alias name is a binding
//! too, bound at each declaration and read bare, scoped, as a parent and in
//! types. A
//! binding with a use that cannot be attributed, such as `:Ready?` or a call
//! `Ready?(1)`, gets no fix rather than a partial one, as does one whose new
//! name the source already spells, which the rename would merge with it.
//!
//! The uses come from a second, lenient parse that accepts suffixed
//! bindings, as the grammar did before ADR-008, and tracks each binding
//! through the parser's own scopes. Every table and every edit of a fix is
//! charged to the caller's work.

use super::{Error, Expr, Name, Node, Parser, Result, Token, Work, name_suffix_position};
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

/// A read of a capitalized name that no local binds, such as `Ready?` in
/// `Ready?.new`, or of a scoped name, such as `Outer::Inner::NAME?`: where its
/// suffix is, the namespace it is read in, the [`Path`] of its scope (0 for a
/// bare name) and its name. A read is not `safe` once it is called.
struct Scoped {
    at: u32,
    owner: Option<u32>,
    path: u32,
    name: Name,
    safe: bool,
}

/// A path of names a scope spells, such as `Outer::Inner`: the path before
/// its last name (0 for none), that name, and the namespace its first name
/// is read in. A path's id is one more than its index, so a path follows
/// the one it extends.
struct Path {
    parent: u32,
    name: Name,
    owner: Option<u32>,
}

/// The uses of suffixed bindings a lenient parse records, each at the
/// offset of the name's first `?` or `!`. A binding's id is one more than
/// the number before it, and a namespace's one more than its index.
#[derive(Default)]
pub(super) struct Uses {
    bindings: u32,
    /// Each constant binding by its namespace and name, as `3:NAME?`; a
    /// reopened class binds it again as the same binding.
    constants: Table<u32>,
    /// Where each binding is bound, with its id.
    sites: Buffer<(u32, u32)>,
    /// Where each binding is read, with its id.
    reads: Buffer<(u32, u32)>,
    scoped: Buffer<Scoped>,
    /// The names types spell, read as [`Scoped`] ones; a lookahead may
    /// read as a type what the parser then reads as a value.
    types: Buffer<Scoped>,
    /// Each class, module and enum by its parent's id (0 at the top level)
    /// and name, as `3:Inner`; a reopened one keeps its id.
    namespace_ids: Table<u32>,
    parents: Buffer<u32>,
    names: Buffer<Name>,
    /// The binding each namespace's suffixed name is, or 0.
    namespace_bindings: Buffer<u32>,
    /// The bindings a symbol may name, as `:Ready?` names a class for
    /// `is_type?` and `:draft?` an enum member, with their names.
    nameable: Buffer<(u32, Name)>,
    /// Each suffixed symbol in the source, as [`symbol_key`] spells it.
    symbols: Table<()>,
    /// Every name the source spells, in code, symbols and interpolations,
    /// or none when it does not lex.
    spelled: Option<Table<()>>,
    /// Each capitalized name a nullable type spells, as `Ready` in
    /// `Ready?`, by the offset of its `?`.
    nullable: Buffer<(u32, Name)>,
    /// Each binding a scoped read may reach, a constant, an enum member or
    /// a nested class or module, with its name.
    members: Buffer<(u32, Name)>,
    /// The names read through a scope that is no path of names, such as
    /// `LIMIT!` in `list.first::LIMIT!`.
    opaque: Table<()>,
    /// Where the lenient parse failed, after which no use is known.
    failed_at: Option<usize>,
    paths: Buffer<Path>,
    /// Each path by the offset of its last name.
    path_at: Table<u32>,
    /// By the offset a scoped expression starts at, the path the latest one
    /// there spells, and one more than the index of the latest read there,
    /// or 0, which a call may turn out to call.
    scopes: Table<u32>,
    calls: Table<u32>,
    /// Each use recorded, by its kind and offset, so a lookahead that parses
    /// a name again records it once, with the read's index.
    recorded: Table<u32>,
}

/// The spelling a symbol and a name compare by: lowercase, without `_`.
fn symbol_key(work: &dyn Work, name: &str) -> Result<Name> {
    // Lowercasing may lengthen a character, up to three times over.
    let _reserved = work.reserve(name.len().saturating_mul(3))?;
    let folded: String = name
        .chars()
        .filter(|&c| c != '_')
        .flat_map(char::to_lowercase)
        .collect();
    Name::new(work, &folded)
}

/// The key of `name` within the namespace `parent`, 0 at the top level.
fn key(work: &dyn Work, parent: u32, name: &str) -> Result<Name> {
    Name::join(work, &[&parent.to_string(), ":", name])
}

/// The key of an offset in the tables that record by one.
fn at_key(work: &dyn Work, kind: &str, at: u32) -> Result<Name> {
    let mut digits = [0u8; 11];
    let mut start = digits.len();
    let mut at = at;
    loop {
        start -= 1;
        digits[start] = b'0' + (at % 10) as u8;
        at /= 10;
        if at == 0 {
            break;
        }
    }
    let digits = std::str::from_utf8(&digits[start..]).unwrap_or_default();
    Name::join(work, &[kind, digits])
}

impl Uses {
    fn binding(
        &mut self,
        work: &dyn Work,
        name: &str,
        namespace: Option<u32>,
        at: u32,
    ) -> Result<u32> {
        let key = match namespace {
            Some(namespace) => {
                let key = key(work, namespace, name)?;
                if let Some(&id) = self.constants.get(work, &key)? {
                    self.site(work, at, id)?;
                    return Ok(id);
                }
                Some(key)
            }
            None => None,
        };
        self.bindings = self.bindings.saturating_add(1);
        let id = self.bindings;
        self.site(work, at, id)?;
        if let Some(key) = key {
            self.constants.insert(work, key, id)?;
            self.members.push(work, (id, Name::new(work, name)?))?;
        }
        Ok(id)
    }

    /// Records a place `id` is bound, once.
    fn site(&mut self, work: &dyn Work, at: u32, id: u32) -> Result<()> {
        if self
            .recorded
            .insert(work, at_key(work, "b", at)?, 0)?
            .is_none()
        {
            self.sites.push(work, (at, id))?;
        }
        Ok(())
    }

    /// Records a read of the local `id`, once.
    fn local_read(&mut self, work: &dyn Work, at: u32, id: u32) -> Result<()> {
        if self
            .recorded
            .insert(work, at_key(work, "l", at)?, 0)?
            .is_none()
        {
            self.reads.push(work, (at, id))?;
        }
        Ok(())
    }

    /// Records a read of a value, once, returning one more than its index.
    fn value_read(&mut self, work: &dyn Work, read: Scoped) -> Result<u32> {
        let key = at_key(work, "s", read.at)?;
        if let Some(&index) = self.recorded.get(work, &key)? {
            return Ok(index);
        }
        self.scoped.push(work, read)?;
        let index = u32::try_from(self.scoped.len()).unwrap_or(u32::MAX);
        self.recorded.insert(work, key, index)?;
        Ok(index)
    }

    /// Records a read of a type's name, once for each of its readings.
    fn type_read(&mut self, work: &dyn Work, read: Scoped) -> Result<()> {
        let kind = if read.safe { "t" } else { "u" };
        if self
            .recorded
            .insert(work, at_key(work, kind, read.at)?, 0)?
            .is_none()
        {
            self.types.push(work, read)?;
        }
        Ok(())
    }

    /// The path `name`, spelled at `at` after the path `parent` (0 for
    /// none), makes: one for each place, however often it is parsed.
    fn path(
        &mut self,
        work: &dyn Work,
        parent: u32,
        name: &str,
        owner: Option<u32>,
        at: u32,
    ) -> Result<u32> {
        let key = at_key(work, "", at)?;
        if let Some(&id) = self.path_at.get(work, &key)? {
            return Ok(id);
        }
        let owner = owner.filter(|_| parent == 0);
        let name = Name::new(work, name)?;
        self.paths.push(
            work,
            Path {
                parent,
                name,
                owner,
            },
        )?;
        let id = u32::try_from(self.paths.len()).unwrap_or(u32::MAX);
        self.path_at.insert(work, key, id)?;
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
                uses.site(self.work, suffix, id)?;
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
            let id = uses.binding(self.work, name, Some(namespace), suffix)?;
            uses.nameable
                .push(self.work, (id, Name::new(self.work, name)?))?;
            let member = at_key(self.work, "m", suffix)?;
            uses.recorded.insert(self.work, member, 0)?;
        }
        Ok(())
    }

    /// In a lenient parse, the id of the class, module, enum or type alias
    /// `name`, spelled at `at`, declared in the current namespace; a reopened
    /// one keeps its id, and a suffixed name is a binding declared again.
    pub(super) fn namespace_entered(&self, name: &str, at: usize) -> Result<Option<u32>> {
        if !lenient() {
            return Ok(None);
        }
        let parent = self.namespace.unwrap_or(0);
        let suffix = suffix_at(self.source, name, at);
        let mut uses = self.suffixed.borrow_mut();
        let key = key(self.work, parent, name)?;
        if let Some(&id) = uses.namespace_ids.get(self.work, &key)? {
            let binding = uses.namespace_bindings[id as usize - 1];
            if let (Some(suffix), true) = (suffix, binding != 0) {
                uses.site(self.work, suffix, binding)?;
            }
            return Ok(Some(id));
        }
        uses.parents.push(self.work, parent)?;
        uses.names.push(self.work, Name::new(self.work, name)?)?;
        let id = u32::try_from(uses.parents.len()).unwrap_or(u32::MAX);
        uses.namespace_ids.insert(self.work, key, id)?;
        let binding = match suffix {
            Some(suffix) => {
                let binding = uses.binding(self.work, name, None, suffix)?;
                uses.nameable
                    .push(self.work, (binding, Name::new(self.work, name)?))?;
                if parent != 0 {
                    uses.members
                        .push(self.work, (binding, Name::new(self.work, name)?))?;
                }
                binding
            }
            None => 0,
        };
        uses.namespace_bindings.push(self.work, binding)?;
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
        let mut uses = self.suffixed.borrow_mut();
        if let Some(&id) = self.locals.get(self.work, name)? {
            if id != 0 {
                uses.local_read(self.work, suffix, id)?;
            }
            return Ok(());
        }
        // A capitalized name no local binds may name a class, module or enum.
        if name.starts_with(super::unicode::upper) {
            let entry = Scoped {
                at: suffix,
                owner: self.namespace,
                path: 0,
                name: Name::new(self.work, name)?,
                safe: true,
            };
            uses.value_read(self.work, entry)?;
        }
        Ok(())
    }

    /// Records the read of `name`, spelled at `at` through `scope`, and,
    /// when a scope `continues` them, the path the two spell if `scope` is
    /// one, such as `Outer::Inner`. A read `called` with arguments, as in
    /// `A::B!(1)`, is of a method that merely shares a name, so not safe.
    pub(super) fn scoped_read(
        &self,
        scope: &Expr,
        name: &str,
        at: usize,
        continues: bool,
        called: bool,
    ) -> Result<()> {
        if !lenient() {
            return Ok(());
        }
        let work = self.work;
        let suffix = suffix_at(self.source, name, at);
        if suffix.is_none() && !continues {
            return Ok(());
        }
        let start = at_key(work, "", scope.offset)?;
        let mut uses = self.suffixed.borrow_mut();
        let path = match &scope.node {
            Node::Var(head) => uses.path(work, 0, head, self.namespace, scope.offset)?,
            Node::Scope(_, _, None) => uses.scopes.get(work, &start)?.copied().unwrap_or(0),
            _ => 0,
        };
        let mut index = 0;
        if let Some(suffix) = suffix {
            if path == 0 {
                uses.opaque.insert(work, Name::new(work, name)?, ())?;
            } else {
                let entry = Scoped {
                    at: suffix,
                    owner: self.namespace,
                    path,
                    name: Name::new(work, name)?,
                    safe: !called,
                };
                index = uses.value_read(work, entry)?;
                if called {
                    // A lookahead may have recorded the read already.
                    uses.scoped[index as usize - 1].safe = false;
                }
            }
        }
        uses.calls.insert(work, start.clone(), index)?;
        if continues {
            let at = u32::try_from(at).unwrap_or(u32::MAX);
            let spelled = match path {
                0 => 0,
                path => uses.path(work, path, name, None, at)?,
            };
            uses.scopes.insert(work, start, spelled)?;
        }
        Ok(())
    }

    /// Records a name a type spells at `at` as `written`, after the path
    /// `path` (0 for its first name), returning the path they make when a
    /// name `continues` it. The type reads a final `?` as nullable, so
    /// `Ready!?` names `Ready!`; a binding spelled `Ready!?` it may have
    /// meant instead is not safe to rename.
    pub(super) fn type_read(
        &self,
        path: u32,
        written: &str,
        at: usize,
        continues: bool,
    ) -> Result<u32> {
        if !lenient() {
            return Ok(0);
        }
        let read = written.strip_suffix('?').unwrap_or(written);
        let mut uses = self.suffixed.borrow_mut();
        if read.len() < written.len() && read.starts_with(super::unicode::upper) {
            let question = u32::try_from(at + read.len()).unwrap_or(u32::MAX);
            let key = at_key(self.work, "n", question)?;
            if uses.recorded.insert(self.work, key, 0)?.is_none() {
                let read = Name::new(self.work, read)?;
                uses.nullable.push(self.work, (question, read))?;
            }
        }
        if path != 0 || written.starts_with(super::unicode::upper) {
            let ambiguous = read.len() < written.len() && read.ends_with(['?', '!']);
            for (name, safe) in [(read, true), (written, false)] {
                if !safe && !ambiguous {
                    break;
                }
                let Some(suffix) = suffix_at(self.source, name, at) else {
                    continue;
                };
                let entry = Scoped {
                    at: suffix,
                    owner: self.namespace,
                    path,
                    name: Name::new(self.work, name)?,
                    safe,
                };
                uses.type_read(self.work, entry)?;
            }
        }
        if !continues {
            return Ok(0);
        }
        let at = u32::try_from(at).unwrap_or(u32::MAX);
        uses.path(self.work, path, read, self.namespace, at)
    }

    /// Marks the read `callee` names as unsafe to rename once it turns out
    /// to be called, since a class, module or enum takes no arguments: the
    /// call is of a method that merely shares the name.
    pub(super) fn suffix_call(&self, callee: &Expr) -> Result<()> {
        let name = match &callee.node {
            Node::Var(name) | Node::Scope(_, name, None) if lenient() => name,
            _ => return Ok(()),
        };
        if !name.ends_with(['?', '!']) {
            return Ok(());
        }
        // A bare callee's read is the one at its suffix; a scoped one's, the
        // latest where it starts.
        let key = match &callee.node {
            Node::Var(_) => match suffix_at(self.source, name, callee.offset as usize) {
                Some(at) => at_key(self.work, "s", at)?,
                None => return Ok(()),
            },
            _ => at_key(self.work, "", callee.offset)?,
        };
        let mut uses = self.suffixed.borrow_mut();
        let index = match &callee.node {
            Node::Var(_) => uses.recorded.get(self.work, &key)?,
            _ => uses.calls.get(self.work, &key)?,
        };
        if let Some(&index) = index
            && index != 0
            && uses.scoped[index as usize - 1].name == **name
        {
            uses.scoped[index as usize - 1].safe = false;
        }
        Ok(())
    }

    /// Reads a class's parent, `< Outer::Name`, from its `<`, recording its
    /// suffixed names as scoped reads.
    pub(super) fn inherited(&mut self) -> Result<()> {
        self.bump()?;
        let mut path = 0;
        loop {
            self.pos = self.significant(self.pos);
            if !self.ident(self.pos) {
                return self.expected(super::Label::Text("identifier"));
            }
            let at = self.tokens[self.pos].offset;
            let Token::Word(word) = self.bump()? else {
                unreachable!()
            };
            let mut uses = self.suffixed.borrow_mut();
            if let Some(suffix) = suffix_at(self.source, &word, at) {
                let entry = Scoped {
                    at: suffix,
                    owner: self.namespace,
                    path,
                    name: Name::new(self.work, &word)?,
                    safe: true,
                };
                uses.value_read(self.work, entry)?;
            }
            let at = u32::try_from(at).unwrap_or(u32::MAX);
            path = uses.path(self.work, path, &word, self.namespace, at)?;
            drop(uses);
            if self.token() != &Token::Op("::") {
                return Ok(());
            }
            self.bump()?;
        }
    }
}

/// Parses `source` accepting suffixed bindings, returning their uses, as far
/// as it parses that way.
fn uses(source: &str, work: &dyn Work) -> Result<Uses> {
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
        Err(error) if error.kind == crate::ErrorKind::Syntax => {
            return Ok(Uses {
                failed_at: Some(0),
                ..Uses::default()
            });
        }
        Err(error) => return Err(error),
    };
    let parsing = super::Parsing::<super::recovery::FailFast>::new(parser);
    let failed_at = match parsing.run(super::Call::Program) {
        Ok(_) => None,
        Err(error) if error.kind == crate::ErrorKind::Syntax => Some(error.offset.unwrap_or(0)),
        Err(error) => return Err(error),
    };
    let parser = parsing.parser.into_inner();
    let mut uses = parser.suffixed.into_inner();
    uses.failed_at = failed_at;
    // A capitalized name read before the parser saw it assigned is bound
    // there, not read.
    let mut bound = Table::new();
    for &(at, _) in uses.sites.iter() {
        bound.insert(work, Name::new(work, &at.to_string())?, ())?;
    }
    let mut reads = Buffer::with_capacity(work, uses.scoped.len())?;
    for read in std::mem::take(&mut uses.scoped) {
        if !bound.contains(work, &read.at.to_string())? {
            reads.push(work, read)?;
        }
    }
    uses.scoped = reads;
    // A type's name where a value's is read was only a lookahead.
    let mut values = Table::new();
    let positions = uses.reads.iter().map(|&(at, _)| at);
    for at in positions.chain(uses.scoped.iter().map(|read| read.at)) {
        values.insert(work, Name::new(work, &at.to_string())?, ())?;
    }
    for read in std::mem::take(&mut uses.types) {
        if !values.contains(work, &read.at.to_string())? {
            uses.scoped.push(work, read)?;
        }
    }
    let mut spelled = Table::new();
    let lexemes = parser.tokens.range(0..parser.tokens.len());
    spell(work, source, lexemes, &mut uses, &mut spelled)?;
    // A nullable type names its class without the `?`, unless it was a
    // lookahead at a value.
    for (question, name) in std::mem::take(&mut uses.nullable) {
        work.charge(1)?;
        if !values.contains(work, &question.to_string())? && !spelled.contains(work, &name)? {
            spelled.insert(work, name, ())?;
        }
    }
    uses.spelled = Some(spelled);
    Ok(uses)
}

/// Records every name `lexemes` spell, within interpolations too, with
/// each suffixed symbol's [`symbol_key`].
fn spell<'t, 's: 't>(
    work: &dyn Work,
    source: &str,
    lexemes: impl Iterator<Item = &'t super::lexer::Lexeme<'s>>,
    uses: &mut Uses,
    spelled: &mut Table<()>,
) -> Result<()> {
    use super::lexer::Part;
    fn record(work: &dyn Work, spelled: &mut Table<()>, name: &str) -> Result<()> {
        if !spelled.contains(work, name)? {
            spelled.insert(work, Name::new(work, name)?, ())?;
        }
        Ok(())
    }
    // The tokens before the current one, the latest first.
    let mut recent: [Option<&Token<'_>>; 3] = [None; 3];
    for lexeme in lexemes {
        work.charge(1)?;
        let token = &lexeme.token;
        match token {
            Token::Word(word) => {
                // The lexer ends a name before a `?` or `!` it splits off, as
                // in `x?=1`, where the name is spelled with it. `a!=b` still
                // compares `a`, except as a setter's name after `def`.
                let rest = source.get(lexeme.end..).unwrap_or_default();
                let run = rest.len() - rest.trim_start_matches(['?', '!']).len();
                let setter = match recent {
                    [Some(Token::Word(def)), ..] => **def == *"def",
                    [
                        Some(Token::P('.')),
                        Some(Token::Word(receiver)),
                        Some(Token::Word(def)),
                    ] => **receiver == *"self" && **def == *"def",
                    _ => false,
                };
                let operator = rest.starts_with("!=") || rest.starts_with("!~");
                let written = source.get(lexeme.offset..lexeme.end + run);
                match written {
                    Some(written) if run > 0 && (setter || !operator) => {
                        record(work, spelled, written)?
                    }
                    _ => record(work, spelled, word)?,
                }
            }
            Token::Symbol(_) | Token::QuotedSymbol(_) => {
                let name = match token {
                    Token::Symbol(name) => name.as_bytes(),
                    Token::QuotedSymbol(name) => name.as_ref(),
                    _ => unreachable!(),
                };
                if let Ok(name) = std::str::from_utf8(name) {
                    if name.ends_with(['?', '!']) {
                        let key = symbol_key(work, name)?;
                        uses.symbols.insert(work, key, ())?;
                    }
                    record(work, spelled, name)?;
                }
            }
            Token::Template(_) | Token::Words(_) => {
                let (entries, symbol): (&[Buffer<Part<'_>>], bool) = match token {
                    Token::Template(parts) => (std::slice::from_ref(parts), false),
                    Token::Words(words) => (&words.entries, words.symbol),
                    _ => unreachable!(),
                };
                for entry in entries {
                    for part in entry.iter() {
                        match part {
                            Part::Text(text) if symbol => {
                                if let Ok(text) = std::str::from_utf8(text) {
                                    record(work, spelled, text)?;
                                }
                            }
                            Part::Expr(inner, _) => {
                                spell(work, source, inner.iter(), uses, spelled)?;
                            }
                            Part::Text(_) => (),
                        }
                    }
                }
            }
            _ => (),
        }
        recent = [Some(token), recent[0], recent[1]];
    }
    Ok(())
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
        Ok(uses) => uses,
        Err(failure) => return failure,
    };
    let mut diagnostics = error.take_diagnostics();
    let charge = match extended(work, source, &uses, &mut diagnostics) {
        Ok(charge) => charge,
        Err(failure) => return failure,
    };
    if let Err(failure) = withhold_collisions(work, source, &uses, &mut diagnostics) {
        return failure;
    }
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
    source: &str,
    uses: &Uses,
    diagnostics: &mut [Diagnostic],
) -> Result<Option<crate::budget::Charge>> {
    let bindings = uses.bindings as usize;
    // Bindings whose uses cannot all be found, which get no fix at all.
    let mut unsafe_ids = Buffer::with_capacity(work, bindings + 1)?;
    for _ in 0..=bindings {
        unsafe_ids.push(work, false)?;
    }
    for (id, name) in uses.nameable.iter() {
        if uses.symbols.contains(work, &symbol_key(work, name)?)? {
            unsafe_ids[*id as usize] = true;
        }
    }
    // Each read that resolves to a class, module, enum or constant, with
    // its bindings.
    let resolved = resolve(work, uses)?;
    let mut scoped = Buffer::new();
    // The reads that make their bindings unsafe.
    let mut unsafe_reads = Buffer::new();
    // A scope the parse cannot resolve, such as a local that holds a class
    // in `a::LIMIT!`, may reach any member of that name.
    let mut opaque = Table::new();
    for (read, &namespace) in uses.scoped.iter().zip(resolved.iter()) {
        work.charge(1)?;
        if namespace == 0 {
            if read.path != 0 {
                opaque.insert(work, read.name.clone(), ())?;
            }
            continue;
        }
        if read.path == 0 {
            // A bare read names the namespace itself.
            let id = uses.namespace_bindings[namespace as usize - 1];
            if id != 0 && read.safe {
                scoped.push(work, (read.at, id))?;
            } else if id != 0 {
                unsafe_ids[id as usize] = true;
                unsafe_reads.push(work, (read.at, id))?;
            }
            continue;
        }
        if let Some(child) = uses.child(work, namespace, &read.name)? {
            let id = uses.namespace_bindings[child as usize - 1];
            if id != 0 && read.safe {
                scoped.push(work, (read.at, id))?;
            } else if id != 0 {
                unsafe_ids[id as usize] = true;
                unsafe_reads.push(work, (read.at, id))?;
            }
        }
        let constant = uses
            .constants
            .get(work, &key(work, namespace, &read.name)?)?;
        if let Some(&id) = constant {
            if read.safe {
                scoped.push(work, (read.at, id))?;
            } else {
                unsafe_ids[id as usize] = true;
                unsafe_reads.push(work, (read.at, id))?;
            }
        }
    }
    for (id, name) in uses.members.iter() {
        work.charge(1)?;
        if opaque.contains(work, name)? || uses.opaque.contains(work, name)? {
            unsafe_ids[*id as usize] = true;
        }
    }
    // Group every use by its binding: count, then place.
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
    // Bindings whose fix renames them whole, or that have none.
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
        let site = sites.get(work, &at)?.copied();
        if let Some(failed_at) = uses.failed_at {
            // Past where the lenient parse failed, no use is known, and a
            // binding bound before it may be read after it: renaming only
            // what was seen could leave a read behind.
            let method = diagnostic.fixes[0].message == REPEATED;
            if bare(source, edit.span.start)
                && !method
                && (site.is_some() || edit.span.start >= failed_at)
            {
                diagnostic.fixes.clear();
                if let Some(id) = site {
                    fixed[id as usize] = true;
                }
            }
            continue;
        }
        let Some(id) = site else {
            continue;
        };
        if unsafe_ids[id as usize] {
            diagnostic.fixes.clear();
            continue;
        }
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
    // A read the parser reports on its own, as it does `Ready!?` in
    // `Ready!?.new`, is renamed with its binding or not at all; its own fix
    // would rename only it.
    let mut read = Table::new();
    for &(at, id) in uses
        .reads
        .iter()
        .chain(scoped.iter())
        .chain(unsafe_reads.iter())
    {
        let at = Name::new(work, &at.to_string())?;
        if read.get(work, &at)?.is_none() {
            read.insert(work, at, id)?;
        }
    }
    for diagnostic in diagnostics.iter_mut() {
        work.charge(1)?;
        if !renames(diagnostic) {
            continue;
        }
        let at = Name::new(work, &diagnostic.fixes[0].edits[0].span.start.to_string())?;
        if sites.get(work, &at)?.is_some() {
            continue;
        }
        if let Some(&id) = read.get(work, &at)? {
            if fixed[id as usize] || unsafe_ids[id as usize] {
                diagnostic.fixes.clear();
            }
        }
    }
    Ok(charge)
}

/// Clears each V0003 fix that would leave a name the source already spells
/// anywhere, or that an earlier fix leaves: applied, it would make two
/// names one, as `x = 1; x? = 2; [x, x?]` would read the same binding
/// twice. So is one whose name its declaration cannot take: a type alias's
/// or an enum's that names a builtin type, or an enum member's that
/// normalizes to the symbol of a name the source spells. Every fix is
/// cleared when the source does not lex, since its names are then unknown.
fn withhold_collisions(
    work: &dyn Work,
    source: &str,
    uses: &Uses,
    diagnostics: &mut [Diagnostic],
) -> Result<()> {
    let spelled = uses.spelled.as_ref();
    // The names the fixes kept so far leave.
    let mut taken: Table<()> = Table::new();
    // The symbols the spelled names normalize to, once a member needs them.
    let mut symbols: Option<Table<()>> = None;
    for diagnostic in diagnostics.iter_mut() {
        work.charge(1)?;
        if diagnostic.code != Code::NAME_SUFFIX || diagnostic.fixes.is_empty() {
            continue;
        }
        let Some(spelled) = spelled else {
            diagnostic.fixes.clear();
            continue;
        };
        let mut left: Buffer<Name> = Buffer::new();
        let mut collides = false;
        for edit in diagnostic.fixes.iter().flat_map(|fix| fix.edits.iter()) {
            work.charge(1)?;
            let (name, _reserved) = destination(work, source, edit)?;
            if left.last().is_some_and(|last| **last == *name) {
                continue;
            }
            if spelled.contains(work, &name)?
                || taken.contains(work, &name)?
                || !declarable(work, source, uses, spelled, &mut symbols, edit, &name)?
            {
                collides = true;
                break;
            }
            left.push(work, Name::new(work, &name)?)?;
        }
        if collides {
            diagnostic.fixes.clear();
            continue;
        }
        for name in left {
            taken.insert(work, name, ())?;
        }
    }
    Ok(())
}

/// Whether the declaration an edit renames can take `name`: a type alias
/// or an enum cannot take a builtin or prelude type's, an enum member cannot
/// take one that normalizes to the symbol of a name the source spells, as
/// `READY` does to that of a member `Ready`, and a capitalized binding, a
/// constant, cannot rebind a prelude namespace, function or global.
fn declarable(
    work: &dyn Work,
    source: &str,
    uses: &Uses,
    spelled: &Table<()>,
    symbols: &mut Option<Table<()>>,
    edit: &Edit,
    name: &str,
) -> Result<bool> {
    let start = source[..edit.span.start]
        .rfind(|c: char| c != '_' && c != '@' && !super::unicode::letter_or_digit(c))
        .map_or(0, |i| {
            i + source[i..].chars().next().map_or(0, char::len_utf8)
        });
    let keyword = source[..start]
        .trim_end()
        .rsplit(|c: char| !super::unicode::letter_or_digit(c) && c != '_')
        .next()
        .unwrap_or_default();
    let prelude = |types: bool| {
        crate::signatures::table()
            .items
            .iter()
            .any(|item| match item {
                crate::signatures::Item::Alias(alias) => types && alias.name == name,
                crate::signatures::Item::Function(function) => !types && function.name == name,
                crate::signatures::Item::Constant(constant) => !types && constant.name == name,
                crate::signatures::Item::Module(module) => !types && module.name == name,
                _ => false,
            })
    };
    if matches!(keyword, "type" | "enum") {
        return Ok(crate::types::builtin_name(name).is_none() && !prelude(true));
    }
    let at = u32::try_from(edit.span.start).unwrap_or(u32::MAX);
    if !uses.recorded.contains(work, &at_key(work, "m", at)?)? {
        let constant = name.starts_with(super::unicode::upper);
        return Ok(matches!(keyword, "class" | "module") || !constant || !prelude(false));
    }
    if symbols.is_none() {
        let mut normalized = Table::new();
        for (spelling, ()) in spelled.iter(work)? {
            work.charge(1)?;
            let _reserved = work.reserve(spelling.len().saturating_mul(4))?;
            let symbol = crate::enums::symbol(spelling);
            if !normalized.contains(work, &symbol)? {
                normalized.insert(work, Name::new(work, &symbol)?, ())?;
            }
        }
        *symbols = Some(normalized);
    }
    let _reserved = work.reserve(name.len().saturating_mul(4))?;
    let symbol = crate::enums::symbol(name);
    match symbols {
        Some(symbols) => Ok(!symbols.contains(work, &symbol)?),
        None => Ok(true),
    }
}

/// The name an edit of a V0003 fix leaves: the word it edits as it reads
/// once the edit applies, or the name a quoted symbol's replacement spells.
/// A name may be nearly as long as the source, so its memory is reserved,
/// for as long as the reservation returned with it lives, before it is built.
fn destination(
    work: &dyn Work,
    source: &str,
    edit: &Edit,
) -> Result<(String, Option<crate::budget::Charge>)> {
    let (start, end) = (edit.span.start, edit.span.end);
    if let Some(name) = edit
        .replacement
        .strip_prefix(":\"")
        .and_then(|name| name.strip_suffix('"'))
    {
        let reserved = work.reserve(name.len())?;
        return Ok((name.to_string(), reserved));
    }
    let word = |c: char| c == '_' || c == '@' || super::unicode::letter_or_digit(c);
    let from = source[..start].rfind(|c: char| !word(c)).map_or(0, |i| {
        i + source[i..].chars().next().map_or(0, char::len_utf8)
    });
    let to = source[end..]
        .find(|c: char| !word(c) && c != '?' && c != '!')
        .map_or(source.len(), |i| end + i);
    let (before, after) = (&source[from..start], &source[end..to]);
    let length = before.len() + edit.replacement.len() + after.len();
    let reserved = work.reserve(length)?;
    let mut name = String::with_capacity(length);
    name.push_str(before);
    name.push_str(&edit.replacement);
    name.push_str(after);
    Ok((name, reserved))
}

/// Whether the name whose suffix starts at `at` stands bare, where it may
/// name a binding read elsewhere, rather than after a receiver, a sigil, a
/// `:` or `&`, or a keyword that declares a method or accessor.
fn bare(source: &str, at: usize) -> bool {
    let start = source[..at]
        .rfind(|c: char| c != '_' && !super::unicode::letter_or_digit(c))
        .map_or(0, |i| {
            i + source[i..].chars().next().map_or(0, char::len_utf8)
        });
    let before = source[..start].trim_end();
    if before.ends_with(['.', '@', ':', '&']) {
        return false;
    }
    let word = before
        .rsplit(|c: char| c != '_' && !super::unicode::letter_or_digit(c))
        .next()
        .unwrap_or_default();
    !matches!(
        word,
        "def" | "alias" | "alias_method" | "property" | "getter" | "setter"
    )
}

/// The label of a method's repeated suffix fix, which renames no binding.
pub(super) const REPEATED: &str = "remove the repeated name suffix";

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

/// The namespace each read names, when bare, or reads through, or 0 where
/// none does: each path once, from the namespace its first name resolves
/// to, as the checker resolves them.
fn resolve(work: &dyn Work, uses: &Uses) -> Result<Buffer<u32>> {
    // The first names to look up: each path's, then each bare read's.
    let mut heads: Buffer<(&str, Option<u32>)> = Buffer::new();
    for path in uses.paths.iter() {
        if path.parent == 0 {
            heads.push(work, (&path.name, path.owner))?;
        }
    }
    let roots = heads.len();
    for read in uses.scoped.iter() {
        if read.path == 0 {
            heads.push(work, (&read.name, read.owner))?;
        }
    }
    let found = lookup(work, uses, &heads)?;
    let mut namespaces = Buffer::with_capacity(work, uses.paths.len())?;
    let mut root = 0;
    for path in uses.paths.iter() {
        work.charge(1)?;
        let namespace = match path.parent {
            0 => {
                root += 1;
                found[root - 1]
            }
            parent => match namespaces[parent as usize - 1] {
                0 => 0,
                parent => uses.child(work, parent, &path.name)?.unwrap_or(0),
            },
        };
        namespaces.push(work, namespace)?;
    }
    let mut resolved = Buffer::with_capacity(work, uses.scoped.len())?;
    let mut bare = roots;
    for read in uses.scoped.iter() {
        work.charge(1)?;
        let namespace = match read.path {
            0 => {
                bare += 1;
                found[bare - 1]
            }
            path => namespaces[path as usize - 1],
        };
        resolved.push(work, namespace)?;
    }
    Ok(resolved)
}

/// The namespace each first name `(name, owner)` resolves to, or 0: first in
/// the namespace it is read in, then at the top level, then in each
/// enclosing namespace.
fn lookup(work: &dyn Work, uses: &Uses, heads: &[(&str, Option<u32>)]) -> Result<Buffer<u32>> {
    let mut found = Buffer::with_capacity(work, heads.len())?;
    // Names left to the enclosing namespaces, by the one they start from.
    let mut pending = Buffer::new();
    for (index, &(head, owner)) in heads.iter().enumerate() {
        work.charge(1)?;
        let mut namespace = None;
        if let Some(owner) = owner {
            namespace = uses.child(work, owner, head)?;
        }
        if namespace.is_none() {
            namespace = uses.child(work, 0, head)?;
        }
        if let (None, Some(owner)) = (namespace, owner) {
            let parent = uses.parents[owner as usize - 1];
            if parent != 0 {
                pending.push(work, (parent, u32::try_from(index).unwrap_or(u32::MAX)))?;
            }
        }
        found.push(work, namespace.unwrap_or(0))?;
    }
    if !pending.is_empty() {
        enclosing(work, uses, heads, &pending, &mut found)?;
    }
    Ok(found)
}

/// Resolves each `pending` first name, `(start, head)`, in the nearest of
/// the namespace it starts from and those enclosing it, other than the top
/// level. One walk of the namespaces keeps, for every name, a stack of the
/// enclosing namespaces that have a member of that name, so each name costs
/// one lookup however deep it is.
fn enclosing(
    work: &dyn Work,
    uses: &Uses,
    heads: &[(&str, Option<u32>)],
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
            let (head, _) = heads[read as usize];
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
    fn a_fix_name_is_charged_before_it_is_built() {
        let long = 1 << 20;
        let source = format!("{}? = 1", "x".repeat(long));
        let edit = Edit {
            span: Span::new(long, long + 1),
            replacement: String::new(),
        };
        let mut options = unlimited();
        options.limits.memory_bytes = Some(long / 2);
        let mut context = CallContext::new(options);
        let work = Meter(RefCell::new(&mut context));
        let error = destination(&work, &source, &edit).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory, "{error}");
        let error = symbol_key(&work, &source[..long + 1]).unwrap_err();
        assert_eq!(error.kind, ErrorKind::Memory, "{error}");
        let (name, _) = destination(&(), &source, &edit).unwrap();
        assert_eq!(name.len(), long);
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
        let uses = uses(source, &()).unwrap();
        let mut diagnostics = error.take_diagnostics();
        let mut context = CallContext::new(unlimited());
        extended(
            &Meter(RefCell::new(&mut context)),
            source,
            &uses,
            &mut diagnostics,
        )
        .unwrap();
        context.stats().steps
    }

    /// Classes and modules with suffixed names, one read many times and
    /// many read once each, through a scope, bare and in types.
    fn namespaces(n: usize) -> String {
        let mut source = String::from("module Many?\nX = 1\nend\n");
        for i in 0..n {
            source.push_str(&format!("class C{i}!\nend\n"));
        }
        for i in 0..n {
            source.push_str(&format!(
                "def f{i}(c: C{i}!) -> int\nMany?::X\nend\nf{i}(C{i}!.new)\n"
            ));
        }
        source
    }

    /// The steps of the whole fix pass on `source`: its lenient parse, and
    /// the fixes it extends.
    fn pass_steps(source: &str) -> u64 {
        let mut error = super::super::canonical_error_mode(source, &(), true).unwrap();
        let mut diagnostics = error.take_diagnostics();
        let mut context = CallContext::new(unlimited());
        {
            let work = Meter(RefCell::new(&mut context));
            let uses = uses(source, &work).unwrap();
            assert_eq!(uses.failed_at, None, "{source}");
            extended(&work, source, &uses, &mut diagnostics).unwrap();
        }
        context.stats().steps
    }

    /// A binding that starts the fix pass, then `open` `depth` times around
    /// `width` suffixed reads and `close` as often, inside `before` and
    /// `after`.
    fn nest(shape: [&str; 6], depth: usize, width: usize) -> String {
        let [before, open, item, separator, close, after] = shape;
        let mut source = String::from("x? = 1\n");
        source.push_str(before);
        source.push_str(&open.repeat(depth));
        source.push_str(&vec![item; width].join(separator));
        source.push_str(&close.repeat(depth));
        source.push_str(after);
        source + "\n"
    }

    /// Each way the parser nests, with the deepest nesting it allows.
    const SHAPES: [([&str; 6], usize); 9] = [
        (["", "F?(", "A?", ", ", ")", ""], 1000),
        (["", "f do\n", "A?", "\n", "\nend", ""], 300),
        (["", "p({ b: [", "{ a: A! }", ", ", "] })", ""], 300),
        (["", "p([", "A!", ", ", "])", ""], 300),
        (["", "begin\n", "A?", "\n", "\nrescue\nA?\nend", ""], 1000),
        (["", "module M?\n", "M?", "\n", "\nend", ""], 1000),
        (["p(", "\"#{[", "A?", ", ", "]}\"", ")"], 6),
        (["def f(y: ", "array<", "A!", " | ", ">", ")\nend"], 60),
        (
            ["", "class C\nX! = 1\nend\n", "C::X!", "\n", "", ""],
            usize::MAX,
        ),
    ];

    /// A class whose parent is spelled `depth` suffixed names deep, a read
    /// through as deep a path, and `width` reads through a short one.
    fn paths(depth: usize, width: usize) -> String {
        let path: Vec<String> = (0..depth).map(|i| format!("M{i}?")).collect();
        let path = path.join("::");
        format!("x? = 1\nclass Z < {path}\nend\n{path}\n") + &"M0?::M1?::M2?\n".repeat(width)
    }

    type Source<'a> = Box<dyn Fn(usize) -> String + 'a>;

    /// However the source nests, doubling it at most doubles the fix pass.
    fn nesting_leaves_the_fix_pass_linear() {
        // The review's witness, 900 calls deep around 300,000 reads, at a
        // hundredth of its size.
        let witness = |n: usize| nest(SHAPES[0].0, 9 * n / 3000, n);
        let steps = |source: &dyn Fn(usize) -> String| {
            (pass_steps(&source(3000)), pass_steps(&source(6000)))
        };
        let (small, large) = steps(&witness);
        assert!(
            large * 100 <= small * 205,
            "witness: {small} steps, then {large}"
        );
        for (index, &(shape, deepest)) in SHAPES.iter().enumerate() {
            // A shape that nests only a few levels keeps its depth.
            let depth = |n: usize| match deepest {
                ..10 => deepest,
                _ => (n / 10).min(deepest.saturating_mul(n) / 6000),
            };
            let variants: [(&str, Source); 3] = [
                ("deep and wide", Box::new(|n| nest(shape, depth(n), n))),
                ("deep", Box::new(|n| nest(shape, depth(n), 3))),
                ("wide", Box::new(|n| nest(shape, 2.min(deepest), n))),
            ];
            for (variant, source) in &variants {
                let (small, large) = steps(source);
                assert!(
                    large * 100 <= small * 205,
                    "shape {index}, {variant}: {small} steps, then {large}"
                );
            }
        }
        let variants: [(&str, Source); 3] = [
            ("deep", Box::new(|n| paths(n / 10, 3))),
            ("wide", Box::new(|n| paths(3, n))),
            ("deep and wide", Box::new(|n| paths(n / 10, n))),
        ];
        for (variant, source) in &variants {
            let (small, large) = steps(source);
            assert!(
                large * 100 <= small * 205,
                "paths, {variant}: {small} steps, then {large}"
            );
        }
    }

    #[test]
    fn the_fix_pass_grows_linearly() {
        nesting_leaves_the_fix_pass_linear();
        let (error, _, _) = check(&nested(4), unlimited());
        assert_eq!(error.diagnostics()[0].fixes[0].edits.len(), 5, "{error}");
        let (error, _, _) = check(&namespaces(4), unlimited());
        assert_eq!(error.diagnostics()[0].fixes[0].edits.len(), 5, "{error}");
        for source in [
            constants as fn(usize) -> String,
            |n| repeated(1, n),
            |n| nested(n / 10),
            namespaces,
        ] {
            let (small, large) = (fix_steps(&source(2000)), fix_steps(&source(4000)));
            // Doubling the uses at most doubles the work.
            assert!(
                large <= small * 2,
                "{small} steps for 2,000 uses, {large} for 4,000"
            );
        }
        // Across the whole compilation too, where the parser is linear.
        for source in [
            constants as fn(usize) -> String,
            |n| repeated(1, n),
            namespaces,
        ] {
            let steps = |n| check(&source(n), unlimited()).1.steps;
            let (small, large) = (steps(2000), steps(4000));
            assert!(
                large * 100 <= small * 205,
                "{small} steps for 2,000 uses, {large} for 4,000"
            );
        }
    }
}
