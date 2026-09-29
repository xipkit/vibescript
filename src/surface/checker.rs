//! The compiler's use of the rules: every removed spelling in a source as
//! a `V04xx` diagnostic, with the rewrite as its fix.

use super::{Finding, Rule, context::Surface, parse, syntax};
use crate::{
    diagnostic::{Code, Diagnostic, Edit, Fix, Span},
    tooling,
    typing::CallTypes,
};
use std::ops::{Deref, DerefMut};

/// How deeply nested statements and expressions may be before the check
/// moves to a thread with a larger stack. The rules' parser and walk
/// recurse once per level, taking about 2.5 KiB of stack per level in an
/// optimized build and 25 KiB in a debug one.
const NESTING: usize = 48;

/// The stack for sources nested more deeply than [`NESTING`].
#[cfg(not(target_os = "wasi"))]
const DEEP_STACK: usize = 256 << 20;

/// Reports every removed spelling in `source` as a diagnostic, deciding
/// receiver types from the syntax alone. A source that does not parse
/// fails with its syntax error, and one the rules' parser cannot read has
/// none.
#[cfg(test)]
pub fn check(source: &str) -> crate::Result<Vec<Diagnostic>> {
    let tokens = tooling::tokens(source)?;
    Ok(walk(source, &tokens, &CallTypes::default()).unwrap_or_default())
}

/// The removed spellings in `source`, whose tokens the compiler read, in
/// source order, or `None` when the rules' parser cannot read it. `calls`
/// gives the static checker's receiver types, which decide the rename of a
/// member whose replacement depends on its receiver.
fn walk(source: &str, tokens: &[tooling::Token], calls: &CallTypes) -> Option<Vec<Diagnostic>> {
    match parse::parse_tokens(source, tokens, NESTING) {
        Ok(tree) => Some(diagnostics(source, &tree, calls)),
        Err(_) => deep(source, tokens, calls),
    }
}

/// The most the rules' pass holds for each token of a source, measured
/// over the corpora and the checker's adversarial programs: its copy of
/// the token and its share of the syntax tree and of the walk's records.
const PER_TOKEN: usize = 240;

/// The copies the pass holds of each byte of the dotted names of nested
/// classes and modules, which grow with the square of their depth.
const NAME_COPIES: usize = 3;

/// About the most memory [`add_to`] holds while it reads a source with
/// `tokens`, whose interpolations hold what `interpolated` says and whose
/// classes and modules have qualified names of `names` bytes in all, which
/// the checker's memory account adds to its own.
pub(crate) fn footprint(
    tokens: &[tooling::Token],
    interpolated: crate::syntax::Interpolated,
    names: usize,
) -> usize {
    let payloads: usize = tokens
        .iter()
        .map(|token| match &token.kind {
            tooling::TokenKind::String(bytes) => bytes.len(),
            tooling::TokenKind::Template(parts) => {
                parts.len() * std::mem::size_of::<std::ops::Range<usize>>()
            }
            _ => 0,
        })
        .sum();
    (tokens.len() + interpolated.tokens) * PER_TOKEN
        + payloads
        + interpolated.bytes
        + names * NAME_COPIES
}

/// Adds the removed spellings in `source` to a static check's diagnostics,
/// in source order, charging a step for each of its tokens and of the
/// `interpolated` ones inside its interpolations, which it lexes again.
///
/// A static checker's diagnostic inside a removed spelling, such as the
/// unknown member `nil?` or the missing block of `reduce(:+)`, is left
/// out: the spelling's own diagnostic says what replaces it.
pub(crate) fn add_to(
    checked: &mut crate::typing::Checked,
    source: &str,
    tokens: &[tooling::Token],
    interpolated: usize,
) {
    let read = u64::try_from(tokens.len() + interpolated).unwrap_or(u64::MAX);
    checked.steps += read;
    let Some(surface) = walk(source, tokens, &checked.calls) else {
        // The compiler's grammar reads the removed syntax only so that these
        // rules report it. A source they cannot read must parse without
        // it, so that removed syntax never compiles.
        checked.steps += read;
        let error = crate::syntax::canonical_error(source, &());
        // A source both grammars accept that the rules cannot read skips
        // every rule, so the rules' parser has fallen behind the compiler's.
        #[cfg(not(target_os = "wasi"))]
        debug_assert!(
            error.is_some(),
            "the rules' parser rejects a source the compiler accepts: {:?}",
            source.chars().take(400).collect::<String>()
        );
        if let Some(error) = error {
            let at = error.offset.unwrap_or(0);
            checked.diagnostics.push(Diagnostic::error(
                Code::SYNTAX,
                Span::new(at, at),
                error.message,
            ));
        }
        return;
    };
    if surface.is_empty() {
        return;
    }
    checked.diagnostics.retain(|diagnostic| {
        !surface.iter().any(|removed| {
            removed.span.start <= diagnostic.span.start && diagnostic.span.end <= removed.span.end
        })
    });
    checked.diagnostics.extend(surface);
    checked
        .diagnostics
        .sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.span.end));
}

#[cfg(not(target_os = "wasi"))]
fn deep(source: &str, tokens: &[tooling::Token], calls: &CallTypes) -> Option<Vec<Diagnostic>> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(DEEP_STACK)
            .spawn_scoped(scope, || {
                parse::parse_tokens(source, tokens, usize::MAX)
                    .ok()
                    .map(|tree| diagnostics(source, &tree, calls))
            })
            .ok()
            .and_then(|thread| thread.join().ok())
            .flatten()
    })
}

/// WASI preview 1 cannot start a thread with a larger stack, so a source
/// nested this deeply goes unread there.
#[cfg(target_os = "wasi")]
fn deep(_: &str, _: &[tooling::Token], _: &CallTypes) -> Option<Vec<Diagnostic>> {
    None
}

fn diagnostics(source: &str, tree: &syntax::Tree, calls: &CallTypes) -> Vec<Diagnostic> {
    let mut checker = Checker {
        surface: Surface::new(source, tree),
        calls,
        findings: Vec::new(),
    };
    checker.program(&tree.body);
    let mut diagnostics = Vec::new();
    for (group, rewrite) in checker.surface.rewrites.iter().enumerate() {
        let code = rewrite.rule.code();
        let edits: Vec<Edit> = checker
            .surface
            .edits
            .flatten(source, group)
            .into_iter()
            .map(|(at, replacement)| Edit {
                span: span(at),
                replacement,
            })
            .filter(|edit| edit.span.start < edit.span.end || !edit.replacement.is_empty())
            .collect();
        let message = match rewrite.rule {
            Rule::Require => format!(
                "`require` names modules and aliases with string literals, not `{}`; {}",
                rewrite.removed, rewrite.advice
            ),
            _ => format!("`{}` was removed; {}", rewrite.removed, rewrite.advice),
        };
        let mut diagnostic = Diagnostic::error(code, span(rewrite.span), message);
        let fix = Fix::edits(rewrite.advice.clone(), edits);
        if !fix.edits.is_empty() && fix.apply(source).is_some() {
            diagnostic = diagnostic.with_fix(fix);
        }
        diagnostics.push(diagnostic);
    }
    for finding in &checker.findings {
        let code = finding.rule.code();
        let message = format!("`{}` was removed; {}", finding.removed, finding.advice);
        let mut diagnostic = Diagnostic::error(code, span(finding.span), message);
        if !finding.suggestion.is_empty() {
            let edits = finding
                .suggestion
                .iter()
                .map(|(at, replacement)| Edit {
                    span: span(*at),
                    replacement: replacement.clone(),
                })
                .collect();
            let fix = Fix::edits(finding.advice.clone(), edits).suggestion();
            if fix.apply(source).is_some() {
                diagnostic = diagnostic.with_fix(fix);
            }
        }
        diagnostics.push(diagnostic);
    }
    diagnostics
        .sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.span.end, diagnostic.code));
    diagnostics
}

fn span(span: syntax::Span) -> Span {
    Span::new(span.start, span.end)
}

/// The compiler's walk: receiver types come from the static checker.
pub(super) struct Checker<'a> {
    surface: Surface<'a>,
    calls: &'a CallTypes,
    findings: Vec<Finding>,
}

impl<'a> Deref for Checker<'a> {
    type Target = Surface<'a>;

    fn deref(&self) -> &Surface<'a> {
        &self.surface
    }
}

impl DerefMut for Checker<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.surface
    }
}

impl<'a> Checker<'a> {
    /// The static checker's receiver type for a member call.
    fn receiver(&self, call: &syntax::Call) -> Option<&'a crate::typing::ReceiverType> {
        self.calls
            .receiver_at(self.surface.tokens[call.name_tok].start)
    }

    /// Takes a finding the rules could not turn into a rewrite.
    pub(super) fn report(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    /// The kinds of value a member call's receiver has, as the rename table
    /// names them (`string`, `array`, `hash`, `nil`, `class Name`, `enum`,
    /// `any` and so on), when the static checker knows them.
    pub(super) fn receiver_kinds(
        &self,
        _: &'a syntax::Expr,
        call: &'a syntax::Call,
    ) -> Option<Vec<String>> {
        let receiver = self.receiver(call)?;
        // An `any` receiver is as unknown as an untyped one, and a declared
        // capability's methods are the host's, whatever their names.
        if receiver
            .bases()
            .iter()
            .any(|base| matches!(base.as_str(), "any" | "host"))
        {
            return None;
        }
        // An optional receiver must be narrowed before a call, which the
        // checker reports; the spelling is decided by what it narrows to.
        let bases = receiver.bases();
        let present = bases.iter().any(|base| base != "nil");
        Some(
            bases
                .iter()
                .filter(|base| !present || *base != "nil")
                .map(|base| match base.as_str() {
                    "array" | "hash" | "int" | "float" | "string" | "bool" | "nil" | "symbol"
                    | "time" | "duration" | "money" | "range" | "regex" | "match_data"
                    | "error" | "any" | "type" | "namespace" => base.clone(),
                    name if self.declared.enums.contains_key(name) => "enum".to_owned(),
                    name => format!("class {name}"),
                })
                .collect(),
        )
    }

    /// Whether the receiver is known to be a value whose member of the
    /// call's name is called the same with or without parentheses: not a
    /// hash, which may hold a function under the name, and not `nil`.
    pub(super) fn receiver_plain(&self, _: &'a syntax::Expr, call: &'a syntax::Call) -> bool {
        self.receiver(call).is_some_and(|receiver| {
            !receiver.bases().is_empty()
                && !receiver
                    .bases()
                    .iter()
                    .any(|base| matches!(base.as_str(), "hash" | "nil" | "any" | "host"))
        })
    }

    /// Whether the receiver is known to be a host value, such as a
    /// capability, whose own methods may be named like removed ones: some
    /// when the receiver's types are known, none when not.
    pub(super) fn receiver_dynamic(
        &self,
        _: &'a syntax::Expr,
        call: &'a syntax::Call,
    ) -> Option<bool> {
        if let Some(receiver) = self.receiver(call) {
            return Some(
                receiver
                    .bases()
                    .iter()
                    .any(|base| matches!(base.as_str(), "any" | "host")),
            );
        }
        let receiver = call.receiver.as_ref()?;
        self.host_rooted(receiver).then_some(true)
    }

    /// Whether every receiver alternative resolves this call to a user method.
    pub(super) fn receiver_owns_method(&self, _: &'a syntax::Expr, call: &'a syntax::Call) -> bool {
        self.calls
            .user_method_at(self.surface.tokens[call.name_tok].start)
    }
}
