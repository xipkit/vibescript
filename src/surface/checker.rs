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
    Ok(walk(
        source,
        &tokens,
        &CallTypes::default(),
        &mut || true,
        &|| false,
    )
    .unwrap_or_default())
}

/// The removed spellings in `source`, whose tokens the compiler read, in
/// source order, or `None` when the rules' parser cannot read it. `calls`
/// gives the static checker's receiver types, which decide the rename of a
/// member whose replacement depends on its receiver. A source nested past
/// [`NESTING`] is parsed again without the limit only if `afford`, which
/// charges that parse, allows it. A parse, and the walk that finds the
/// removed spellings, give up, and the walk reads nothing, once `stop`
/// says the compilation has stopped.
fn walk<'s>(
    source: &'s str,
    tokens: &[tooling::Token],
    calls: &CallTypes,
    afford: &mut dyn FnMut() -> bool,
    stop: parse::Stop<'s>,
) -> Option<Vec<Diagnostic>> {
    match parse::parse_tokens(source, tokens, NESTING, stop) {
        Ok(tree) => diagnostics(source, &tree, calls, stop),
        // A parse that never reached the limit fails the same way without it.
        Err(fail) if fail.too_deep && afford() => deep(source, tokens, calls, stop),
        Err(_) => None,
    }
}

/// The most the rules' pass holds for each token of a source, measured
/// over the corpora and the checker's adversarial programs: its copy of
/// the token and its share of the syntax tree and of the walk's records.
const PER_TOKEN: usize = 240;

/// The copies the pass holds of each byte of the dotted names of nested
/// classes and modules, which grow with the square of their depth.
const NAME_COPIES: usize = 3;

/// The copies the pass holds at once of each byte of an identifier, for
/// each time the source names it: its syntax tree's node and, for a name
/// it declares, its entries in the parser's locals and their journal and
/// in the walk's scope, measured over long names of locals, parameters and
/// methods.
const WORD_COPIES: usize = 4;

/// The copies the pass holds at once of each byte of a percent literal's
/// entries, which it always rewrites as an array literal, beyond its copy
/// of the token: the entries quoted, the array literal, and the edits and
/// renderings of the fix, measured over percent literals of long entries.
const REWRITE_COPIES: usize = 6;

/// About the most memory [`add_to`] holds while it reads a source with
/// `tokens`, whose interpolations hold what `interpolated` says and whose
/// classes and modules have qualified names of `names` bytes in all, which
/// the checker's memory account adds to its own.
pub(crate) fn footprint(
    tokens: &[tooling::Token],
    interpolated: crate::syntax::Interpolated,
    names: usize,
) -> usize {
    // What the tokens hold, which the pass copies: strings, symbols' names,
    // interpolations' spans and percent literals' entries.
    let payloads: usize = tokens.iter().map(crate::typing::Heap::heap).sum();
    let rewritten: usize = tokens
        .iter()
        .map(|token| match &token.kind {
            tooling::TokenKind::Words { entries, .. } => {
                entries.iter().flatten().map(Vec::len).sum()
            }
            _ => 0,
        })
        .sum();
    // The identifiers, which the pass copies from the source.
    let words: usize = tokens
        .iter()
        .filter(|token| token.kind == tooling::TokenKind::Word)
        .map(|token| token.span.len())
        .sum();
    (tokens.len() + interpolated.tokens) * PER_TOKEN
        + payloads
        + (words + interpolated.words) * WORD_COPIES
        + rewritten * REWRITE_COPIES
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
///
/// Each parse after the first, of a source nested too deeply for the first
/// or one the rules cannot read, is charged the same steps, and taken only
/// if `within` says the steps it brings the check to are within its
/// budget. Otherwise the check stops, and compilation fails charging them.
/// A parse also asks `within` now and then whether the budget has run out
/// since, as a deadline or a cancellation does, and gives up if it has.
/// The compiler's own parse of a source the rules cannot read runs through
/// `canonical`, given the steps charged so far, which gives its first
/// syntax error, if any, or `None` once the budget stops it.
pub(crate) fn add_to(
    checked: &mut crate::typing::Checked,
    source: &str,
    tokens: &[tooling::Token],
    interpolated: usize,
    within: &(dyn Fn(u64) -> bool + Sync),
    canonical: &dyn Fn(&str, u64) -> Option<Option<crate::Error>>,
) {
    let read = u64::try_from(tokens.len() + interpolated).unwrap_or(u64::MAX);
    checked.steps += read;
    let charged = checked.steps;
    let stop = move || !within(charged);
    let mut steps = checked.steps;
    let mut afford = || {
        steps = steps.saturating_add(read);
        within(steps)
    };
    let walked = walk(source, tokens, &checked.calls, &mut afford, &stop);
    let Some(surface) = walked else {
        // The compiler's grammar reads the removed syntax only so that these
        // rules report it. A source they cannot read must parse without
        // it, so that removed syntax never compiles.
        let affordable = afford();
        checked.steps = steps;
        if !affordable {
            checked.stopped = true;
            return;
        }
        let Some(error) = canonical(source, steps) else {
            checked.stopped = true;
            return;
        };
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
    checked.steps = steps;
    if surface.is_empty() {
        return;
    }
    // Leaving out the checker's diagnostics a removed spelling contains,
    // and putting the two lists in order, is a step for each diagnostic,
    // taken only within the budget.
    let merging = (checked.diagnostics.len() + surface.len()) as u64;
    checked.steps = checked.steps.saturating_add(merging);
    if !within(checked.steps) {
        checked.stopped = true;
        return;
    }
    // The spellings are in order of where they start, each with the
    // furthest end of those that start no later, so the spelling that
    // could contain a diagnostic is found by search rather than by trying
    // every one.
    let mut furthest = 0;
    let reach: Vec<(usize, usize)> = surface
        .iter()
        .map(|removed| {
            furthest = furthest.max(removed.span.end);
            (removed.span.start, furthest)
        })
        .collect();
    checked.diagnostics.retain(|diagnostic| {
        let before = reach.partition_point(|&(start, _)| start <= diagnostic.span.start);
        before == 0 || reach[before - 1].1 < diagnostic.span.end
    });
    checked.diagnostics.extend(surface);
    checked
        .diagnostics
        .sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.span.end));
}

#[cfg(not(target_os = "wasi"))]
fn deep<'s>(
    source: &'s str,
    tokens: &[tooling::Token],
    calls: &CallTypes,
    stop: parse::Stop<'s>,
) -> Option<Vec<Diagnostic>> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(DEEP_STACK)
            .spawn_scoped(scope, || {
                parse::parse_tokens(source, tokens, usize::MAX, stop)
                    .ok()
                    .and_then(|tree| diagnostics(source, &tree, calls, stop))
            })
            .ok()
            .and_then(|thread| thread.join().ok())
            .flatten()
    })
}

/// WASI preview 1 cannot start a thread with a larger stack, so a source
/// nested this deeply goes unread there.
#[cfg(target_os = "wasi")]
fn deep(
    _: &str,
    _: &[tooling::Token],
    _: &CallTypes,
    _: parse::Stop<'_>,
) -> Option<Vec<Diagnostic>> {
    None
}

/// The removed spellings in `tree` as diagnostics, or `None` once `stop`,
/// which the walk and the loops after it ask every [`POLL`] statements,
/// expressions or diagnostics, says the compilation has stopped.
fn diagnostics<'s>(
    source: &'s str,
    tree: &syntax::Tree,
    calls: &CallTypes,
    stop: parse::Stop<'s>,
) -> Option<Vec<Diagnostic>> {
    let mut checker = Checker {
        surface: Surface::new(source, tree),
        calls,
        findings: Vec::new(),
        stop,
        visits: 0,
        stopped: false,
    };
    checker.program(&tree.body);
    if checker.stopped {
        return None;
    }
    let groups = checker.surface.edits.groups(checker.surface.rewrites.len());
    let mut diagnostics = Vec::new();
    for (group, rewrite) in checker.surface.rewrites.iter().enumerate() {
        if group % POLL == 0 && stop() {
            return None;
        }
        let code = rewrite.rule.code();
        let edits: Vec<Edit> = checker
            .surface
            .edits
            .flatten_group(source, &groups[group])
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
        if !fix.edits.is_empty() && fix.applies(source) {
            diagnostic = diagnostic.with_fix(fix);
        }
        diagnostics.push(diagnostic);
    }
    for (index, finding) in checker.findings.iter().enumerate() {
        if index % POLL == 0 && stop() {
            return None;
        }
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
            if fix.applies(source) {
                diagnostic = diagnostic.with_fix(fix);
            }
        }
        diagnostics.push(diagnostic);
    }
    if checker.stopped || stop() {
        return None;
    }
    diagnostics
        .sort_by_key(|diagnostic| (diagnostic.span.start, diagnostic.span.end, diagnostic.code));
    Some(diagnostics)
}

/// How many statements, expressions or diagnostics the walk for removed
/// spellings takes between asking whether the compilation has stopped.
const POLL: usize = 256;

fn span(span: syntax::Span) -> Span {
    Span::new(span.start, span.end)
}

/// The compiler's walk: receiver types come from the static checker.
pub(super) struct Checker<'a> {
    surface: Surface<'a>,
    calls: &'a CallTypes,
    findings: Vec<Finding>,
    /// Whether the compilation has stopped, which the walk asks every
    /// [`POLL`] statements and expressions.
    stop: parse::Stop<'a>,
    visits: usize,
    /// Whether the walk gave up because the compilation stopped.
    stopped: bool,
}

impl Checker<'_> {
    /// Counts a statement or an expression the walk visits, asking every
    /// [`POLL`] of them whether the compilation has stopped. Returns
    /// whether the walk should give up.
    pub(super) fn halt(&mut self) -> bool {
        if !self.stopped {
            self.visits += 1;
            if self.visits % POLL == 0 {
                self.stopped = (self.stop)();
            }
        }
        self.stopped
    }
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
