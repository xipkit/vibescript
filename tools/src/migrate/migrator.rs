//! Runs the canonical surface's walk over a parsed source with what the
//! recorded runs observed, adding the rules that need observed types, and
//! renders every rewrite.

use super::{Code, Diagnostic, Migration, Options, observe::Facts, types::Types};
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use vibescript::surface::{
    self, Annotation, Finding, Hooks, Place, Probe, Reason, Surface, Test, Walk,
    member_without_parens,
    patterns::{Callee, Change, Pattern, TemplatePiece},
    syntax::*,
};
use vibescript::tooling::TokenKind;

/// Nesting deeper than the parser's default stack allows runs on a larger one.
#[cfg(not(target_os = "wasi"))]
const STACK: usize = 256 << 20;

/// WASI preview 1 cannot start a thread, so the migration runs on the
/// caller's stack there.
#[cfg(target_os = "wasi")]
pub(crate) fn migrate(source: &str, facts: Option<&Facts>, options: &Options) -> Migration {
    migrate_on_stack(source, facts, options)
}

#[cfg(not(target_os = "wasi"))]
pub(crate) fn migrate(source: &str, facts: Option<&Facts>, options: &Options) -> Migration {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(STACK)
            .spawn_scoped(scope, || migrate_on_stack(source, facts, options))
            .expect("spawn the migration thread")
            .join()
            .unwrap_or_else(|_| Migration {
                source: source.to_owned(),
                changed: false,
                diagnostics: vec![diagnostic(
                    source,
                    Code::Internal,
                    0,
                    "the migration failed on this source; it was left unchanged".to_owned(),
                )],
            })
    })
}

fn unchanged(source: &str, diagnostics: Vec<Diagnostic>) -> Migration {
    Migration {
        source: source.to_owned(),
        changed: false,
        diagnostics,
    }
}

fn migrate_on_stack(source: &str, facts: Option<&Facts>, options: &Options) -> Migration {
    if let Err(error) = super::compat::engine().compile(source) {
        let offset = error.offset.unwrap_or(0);
        let message = format!(
            "does not compile, so it was left unchanged: {}",
            error.message
        );
        return unchanged(
            source,
            vec![diagnostic(source, Code::Unparsed, offset, message)],
        );
    }
    let tree = match surface::parse::parse(source) {
        Ok(tree) => tree,
        Err(fail) => {
            let message = format!("could not be parsed for migration: {}", fail.message);
            return unchanged(
                source,
                vec![diagnostic(source, Code::Internal, fail.offset, message)],
            );
        }
    };
    let mut migrator = Migrator::new(source, &tree, facts, options);
    migrator.program(&tree.body);
    let mut diagnostics = std::mem::take(&mut migrator.diagnostics);
    diagnostics.sort_by_key(|d| (d.offset, d.code));
    diagnostics.dedup();
    if migrator.edits.is_empty() {
        return unchanged(source, diagnostics);
    }
    let output = migrator.edits.apply(source);
    if let Some(span) = migrator.edits.conflicts.first() {
        diagnostics.push(diagnostic(
            source,
            Code::Internal,
            span.start,
            "overlapping rewrites; the source was left unchanged".to_owned(),
        ));
        return unchanged(source, diagnostics);
    }
    // Formatting normalizes line ends and trailing spaces, which would change
    // a string literal that spans lines with them. Rewrites never add such
    // literals, so the original source decides.
    let mut output = if !options.surface_only && literals_survive_formatting(source) {
        crate::format::format(&output)
    } else {
        output
    };
    // Results and parameters no run reached take the types the static
    // checker finds for them.
    if !options.surface_only
        && let Some(inferred) = super::repair::infer(source, &output, facts, &mut diagnostics)
    {
        output = inferred;
    }
    if !options.new_syntax
        && let Err(error) = super::compat::engine().compile(&output)
    {
        diagnostics.push(diagnostic(
            source,
            Code::Internal,
            0,
            format!(
                "the migrated source did not compile ({}); the source was left unchanged",
                error.message
            ),
        ));
        return unchanged(source, diagnostics);
    }
    Migration {
        changed: output != source,
        source: output,
        diagnostics,
    }
}

fn literals_survive_formatting(source: &str) -> bool {
    let Ok(tokens) = vibescript::tooling::tokens(source) else {
        return false;
    };
    tokens.iter().all(|token| {
        let literal = matches!(
            token.kind,
            TokenKind::String(_)
                | TokenKind::Template(_)
                | TokenKind::Regex
                | TokenKind::Words { .. }
                | TokenKind::Symbol { quoted: true, .. }
        );
        let text = &source[token.span.clone()];
        !literal || (!text.contains('\r') && !text.contains(" \n") && !text.contains("\t\n"))
    })
}

pub(crate) fn diagnostic(source: &str, code: Code, offset: usize, message: String) -> Diagnostic {
    let offset = offset.min(source.len());
    let before = &source[..offset];
    let line = before.matches('\n').count() + 1;
    let column = before[before.rfind('\n').map_or(0, |i| i + 1)..]
        .chars()
        .count()
        + 1;
    Diagnostic {
        code,
        offset,
        line,
        column,
        message,
    }
}

/// A migration's walk: the canonical surface's state, and what the
/// recorded runs observed.
pub(crate) struct Migrator<'a> {
    surface: Surface<'a>,
    pub facts: Option<&'a Facts>,
    pub options: &'a Options,
    pub diagnostics: Vec<Diagnostic>,
    /// Conditions by the offset of their branch.
    pub conditions: HashMap<usize, Vec<(usize, &'a Types)>>,
    /// Locals whose first assignment has been seen, by function.
    pub first_assignments: HashSet<(Tok, String)>,
}

impl<'a> Deref for Migrator<'a> {
    type Target = Surface<'a>;

    fn deref(&self) -> &Surface<'a> {
        &self.surface
    }
}

impl DerefMut for Migrator<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.surface
    }
}

impl<'a> Migrator<'a> {
    fn new(
        source: &'a str,
        tree: &'a Tree,
        facts: Option<&'a Facts>,
        options: &'a Options,
    ) -> Self {
        let mut conditions: HashMap<usize, Vec<(usize, &Types)>> = HashMap::new();
        if let Some(facts) = facts {
            for ((report, origin), types) in &facts.conditions {
                conditions
                    .entry(*report)
                    .or_default()
                    .push((*origin, types));
            }
        }
        Self {
            surface: Surface::new(source, tree),
            facts,
            options,
            diagnostics: Vec::new(),
            conditions,
            first_assignments: HashSet::new(),
        }
    }

    /// Reports something the migration leaves for a person.
    pub fn note(&mut self, code: Code, offset: usize, message: impl Into<String>) {
        let diagnostic = diagnostic(self.source, code, offset, message.into());
        self.diagnostics.push(diagnostic);
    }

    /// Whether every recorded start of the function at `offset` bound its
    /// arguments, so no parameter check failed.
    pub fn bindings_passed(&self, offset: usize) -> bool {
        self.facts.is_none_or(|facts| {
            facts.starts.get(&offset).copied().unwrap_or(0)
                == facts.entered.get(&offset).copied().unwrap_or(0)
                || !facts.starts.contains_key(&offset)
        })
    }

    /// The observed receiver types of a member call.
    pub fn receiver_types(&self, expr: &Expr, call: &Call) -> Option<&'a Types> {
        let facts = self.facts?;
        let offset = self.compiler_offset(expr);
        facts.receivers.get(&offset, &call.name)
    }

    /// Whether today's runtime has a rename's replacement: its member for
    /// the receiver's type, and every namespace member it calls.
    fn old_runtime_accepts(&self, pattern: &Pattern, call: Option<&Call>) -> bool {
        let Change::Template(pieces) = &pattern.rewrite else {
            return true;
        };
        // Replacements that today's runtime spells but runs differently.
        let differs = match (pattern.receiver.as_str(), pattern.name.as_str()) {
            // `%` refuses a float operand today, and `Time.local` takes no zone.
            (_, "modulo") | ("Time", "new") => true,
            _ => pieces
                .iter()
                .any(|piece| matches!(piece, TemplatePiece::Text(text) if text.contains("in: "))),
        };
        if differs {
            return false;
        }
        // A member written without parentheses must call on today's runtime.
        if let Some(member) = pattern.target_member()
            && call.is_some_and(|call| call.args.is_none())
            && !matches!(pieces.get(1), Some(TemplatePiece::Text(text)) if text.contains('('))
            && !member_without_parens(&pattern.receiver, member)
        {
            return false;
        }
        let text: String = pieces
            .iter()
            .map(|piece| match piece {
                TemplatePiece::Text(text) => text.as_str(),
                _ => " ",
            })
            .collect();
        let builtins = vibescript::builtins();
        let mut rest = text.as_str();
        while let Some(start) = rest.find(|c: char| c.is_ascii_uppercase()) {
            let word: String = rest[start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            rest = &rest[start + word.len()..];
            let Some(member) = rest.strip_prefix('.') else {
                continue;
            };
            let member: String = member
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '?' | '!'))
                .collect();
            let present = builtins
                .get(&word)
                .and_then(|value| value.as_hash())
                .is_some_and(|fields| {
                    fields
                        .iter()
                        .any(|(key, _)| key.as_bytes() == Some(member.as_bytes()))
                });
            if !present {
                return false;
            }
        }
        if pattern.callee == Callee::Global
            && let Some(TemplatePiece::Text(text)) = pieces.first()
        {
            let name: String = text
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty()
                && name.chars().next().is_some_and(char::is_lowercase)
                && !builtins.contains_key(&name)
            {
                return false;
            }
        }
        let Some(member) = pattern.target_member() else {
            return true;
        };
        if matches!(pattern.receiver.as_str(), "T" | "error") {
            return true;
        }
        vibescript::tooling::member_names()
            .iter()
            .find(|(kind, _)| *kind == pattern.receiver)
            .is_none_or(|(_, names)| names.contains(&member))
    }
}

impl<'a> Hooks<'a> for Migrator<'a> {
    fn report(&mut self, finding: Finding) {
        let Some(reason) = finding.reason else {
            return;
        };
        let code = match reason {
            Reason::Rename => Code::Rename,
            Reason::Receiver => Code::Receiver,
            Reason::Dispatch => Code::Dispatch,
            Reason::Require => Code::Require,
            Reason::HashNew => Code::HashNew,
            Reason::Condition => Code::Condition,
            _ => Code::Syntax,
        };
        self.note(code, finding.span.start, finding.message);
    }

    fn new_syntax(&self) -> bool {
        self.options.new_syntax
    }

    fn receiver_kinds(&self, expr: &'a Expr, call: &'a Call) -> Option<Vec<String>> {
        let types = self.receiver_types(expr, call)?;
        if types.is_empty() {
            return None;
        }
        let mut kinds = Vec::new();
        if types.nil {
            kinds.push("nil".to_owned());
        }
        for scalar in &types.scalars {
            kinds.push(format!("{scalar:?}").to_lowercase());
        }
        if types.array.is_some() {
            kinds.push("array".to_owned());
        }
        if types.hash.is_some() {
            kinds.push("hash".to_owned());
        }
        if types.object {
            kinds.push("object".to_owned());
        }
        for class in &types.classes {
            kinds.push(format!("class {class}"));
        }
        if !types.enums.is_empty() {
            kinds.push("enum".to_owned());
        }
        if types.any {
            kinds.push("any".to_owned());
        }
        Some(kinds)
    }

    fn receiver_plain(&self, expr: &'a Expr, call: &'a Call) -> bool {
        self.receiver_types(expr, call).is_some_and(|types| {
            !types.is_empty() && !types.any && types.hash.is_none() && !types.nil
        })
    }

    fn receiver_dynamic(&self, expr: &'a Expr, call: &'a Call) -> Option<bool> {
        self.receiver_types(expr, call).map(|types| types.any)
    }

    fn field_holds_data(&self, expr: &'a Expr, call: &'a Call) -> bool {
        self.receiver_types(expr, call)
            .and_then(|types| types.hash.as_deref())
            .is_none_or(|shape| {
                shape
                    .fields
                    .get(call.name.as_bytes())
                    .is_some_and(|(types, seen)| *seen == shape.count && !types.any)
            })
    }

    fn receiver_field(&self, expr: &'a Expr, call: &'a Call) -> bool {
        self.receiver_types(expr, call)
            .and_then(|types| types.hash.as_deref())
            .is_some_and(|shape| shape.fields.contains_key(call.name.as_bytes()))
    }

    fn bare_call_kinds(&self, expr: &'a Expr, call: &'a Call) -> Option<Vec<String>> {
        let types = self.receiver_types(expr, call)?;
        let mut kinds: Vec<String> = types
            .scalars
            .iter()
            .map(|scalar| format!("{scalar:?}").to_lowercase())
            .collect();
        if types.array.is_some() {
            kinds.push("array".to_owned());
        }
        Some(kinds)
    }

    fn call_returned(&self, expr: &'a Expr, call: &'a Call) -> bool {
        self.facts.is_none_or(|facts| {
            facts
                .calls
                .get(&self.compiler_offset(expr), &call.name)
                .is_none_or(|(started, returned)| started == returned)
        })
    }

    fn index_is_hash(&self, offset: usize) -> Option<bool> {
        let (receiver, _) = self.facts?.indexes.get(&offset)?;
        Some(
            !(receiver.nil
                || receiver.any
                || receiver.array.is_some()
                || !receiver.scalars.is_empty()
                || !receiver.classes.is_empty()
                || !receiver.enums.is_empty()),
        )
    }

    fn namespace_field(&self, expr: &'a Expr) -> bool {
        self.facts
            .and_then(|facts| facts.results.get(&self.compiler_offset(expr), "new"))
            .is_some_and(|types| {
                types.scalars.len() + usize::from(types.array.is_some()) > 0 || types.any
            })
    }

    fn accepts_rewrite(&self, pattern: &Pattern, call: Option<&'a Call>) -> bool {
        self.options.new_syntax || self.old_runtime_accepts(pattern, call)
    }

    fn annotation_passed(&self, annotation: Annotation<'a, '_>) -> bool {
        match annotation {
            Annotation::Parameter(def) => self.bindings_passed(def.offset(self.tokens)),
            Annotation::Result(def, ty) => self
                .facts
                .and_then(|facts| facts.returns.get(&def.offset(self.tokens)))
                .is_none_or(|types| self.accepts(ty, types)),
            Annotation::Property(class, tok, ty) => {
                let offset = self.tokens[tok].start;
                let field = self.token_text(tok);
                let accepts =
                    |types: Option<&Types>| types.is_none_or(|types| self.accepts(ty, types));
                self.bindings_passed(offset)
                    && accepts(self.facts.and_then(|facts| facts.returns.get(&offset)))
                    && accepts(
                        self.facts
                            .and_then(|facts| facts.instance.get(&class.to_owned(), field)),
                    )
            }
            // A block parameter's check is not observed, so a failure
            // quoting the old spelling cannot be ruled out.
            Annotation::BlockParameter => self.options.surface_only,
        }
    }

    fn keyword_type(&mut self, def: &'a Def, param: &'a Param) -> Option<String> {
        if self.options.surface_only {
            let default = param.default.as_ref()?;
            return surface::literal_type(self, default).map(str::to_owned);
        }
        Some(self.keyword_annotation(def, param))
    }

    fn test(&self, expr: &'a Expr, probe: Probe) -> Test {
        if self.options.surface_only {
            return Test::Bool;
        }
        self.observed_test(expr, probe)
    }

    fn after_assign(&mut self, stmt: &'a Stmt, assign: &'a Assign) {
        if !self.options.surface_only {
            self.logical_assignment(stmt, assign);
            self.declare_local(stmt, assign);
        }
    }

    fn after_binary(
        &mut self,
        expr: &'a Expr,
        op: Tok,
        left: &'a Expr,
        right: &'a Expr,
        place: Place,
    ) {
        if !self.options.surface_only {
            self.binary(expr, op, left, right, place);
        }
    }

    fn before_body(&mut self, def: &'a Def, class: Option<&'a Class>) {
        if !self.options.surface_only {
            self.annotate_def(def, class);
        }
    }

    fn before_class(&mut self, class: &'a Class, name: &str) {
        if !self.options.surface_only {
            self.annotate_class(class, name);
        }
    }

    fn after_case(&mut self, node: &'a Case) {
        if !self.options.surface_only {
            self.enum_case(node);
        }
    }
}
