//! The compiler's use of the rules: every removed spelling in a source as
//! a `V04xx` diagnostic, with the rewrite as its fix.

use super::{Finding, Rule, context::Surface, hooks::Hooks, parse, syntax, walk::Walk};
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
/// receiver types from the syntax alone.
///
/// A source that does not parse fails with its syntax error.
pub fn check(source: &str) -> crate::Result<Vec<Diagnostic>> {
    let tokens = tooling::tokens(source)?;
    Ok(check_tokens(source, &tokens, &CallTypes::default()))
}

/// Reports every removed spelling in `source`, whose tokens the compiler
/// read, as a diagnostic. `calls` gives the static checker's receiver
/// types, which decide the rename of a member whose replacement depends on
/// its receiver.
///
/// Diagnostics are in source order. A source the rules' parser cannot
/// read has none.
pub fn check_tokens(source: &str, tokens: &[tooling::Token], calls: &CallTypes) -> Vec<Diagnostic> {
    walk(source, tokens, calls).unwrap_or_default()
}

/// The removed spellings in `source`, or `None` when the rules' parser
/// cannot read it.
fn walk(source: &str, tokens: &[tooling::Token], calls: &CallTypes) -> Option<Vec<Diagnostic>> {
    match parse::parse_tokens(source, tokens, NESTING) {
        Ok(tree) => Some(diagnostics(source, &tree, calls)),
        Err(_) => deep(source, tokens, calls),
    }
}

/// Adds the removed spellings in `source` to a static check's diagnostics,
/// in source order.
///
/// A static checker's diagnostic inside a removed spelling, such as the
/// unknown member `nil?` or the missing block of `reduce(:+)`, is left
/// out: the spelling's own diagnostic says what replaces it.
pub(crate) fn add_to(
    checked: &mut crate::typing::Checked,
    source: &str,
    tokens: &[tooling::Token],
) {
    checked.steps += u64::try_from(tokens.len()).unwrap_or(u64::MAX);
    let Some(surface) = walk(source, tokens, &checked.calls) else {
        // The compiler's grammar reads the removed syntax only so that these
        // rules report it. A source they cannot read must parse without
        // it, so that removed syntax never compiles.
        checked.steps += u64::try_from(tokens.len()).unwrap_or(u64::MAX);
        if let Some(error) = crate::syntax::canonical_error(source, &()) {
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
        let Some(code) = rewrite.rule.code() else {
            continue;
        };
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
        let Some(code) = finding.rule.and_then(Rule::code) else {
            continue;
        };
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
struct Checker<'a> {
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
}

impl<'a> Hooks<'a> for Checker<'a> {
    fn report(&mut self, finding: Finding) {
        self.findings.push(finding);
    }

    fn receiver_kinds(&self, _: &'a syntax::Expr, call: &'a syntax::Call) -> Option<Vec<String>> {
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

    fn receiver_plain(&self, _: &'a syntax::Expr, call: &'a syntax::Call) -> bool {
        self.receiver(call).is_some_and(|receiver| {
            !receiver.bases().is_empty()
                && !receiver
                    .bases()
                    .iter()
                    .any(|base| matches!(base.as_str(), "hash" | "nil" | "any" | "host"))
        })
    }

    fn receiver_dynamic(&self, _: &'a syntax::Expr, call: &'a syntax::Call) -> Option<bool> {
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

    fn receiver_owns_method(&self, _: &'a syntax::Expr, call: &'a syntax::Call) -> bool {
        self.calls
            .user_method_at(self.surface.tokens[call.name_tok].start)
    }
}
