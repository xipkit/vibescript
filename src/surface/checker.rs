//! The compiler's use of the rules: every removed spelling in a source as
//! a `V04xx` diagnostic, with the rewrite as its fix.

use super::{
    Finding, Rule,
    context::Surface,
    edits::{Room, Written},
    parse, syntax,
};
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
        &Room::default(),
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
/// says the compilation has stopped, or once the text the walk copies and
/// renders would pass its `room`.
fn walk<'s>(
    source: &'s str,
    tokens: &[tooling::Token],
    calls: &CallTypes,
    afford: &mut dyn FnMut() -> bool,
    stop: parse::Stop<'s>,
    room: &Room<'_>,
) -> Option<Vec<Diagnostic>> {
    match parse::parse_tokens(source, tokens, NESTING, stop) {
        Ok(tree) => diagnostics(source, &tree, calls, stop, room),
        // A parse that never reached the limit fails the same way without it.
        Err(fail) if fail.too_deep && afford() => deep(source, tokens, calls, stop, room),
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

/// The most the rules' pass holds for each entry of a percent literal
/// beyond its bytes: its quoted copy's place in the list of them, and its
/// separator in the array literal and in the fix's copies of it, measured
/// over percent literals of many short entries.
const PER_ENTRY: usize = 40;

/// The entries of the percent literals among `tokens`, each of which the
/// rules' pass rewrites, a step each.
pub(crate) fn entries(tokens: &[tooling::Token], stop: parse::Stop<'_>) -> Option<usize> {
    let mut total = 0;
    for token in tokens {
        if stop() {
            return None;
        }
        if let tooling::TokenKind::Words { entries, .. } = &token.kind {
            total += entries.len();
        }
    }
    Some(total)
}

/// The estimated storage of the surface pass, with each sizing visit paced.
pub(crate) fn footprint(
    tokens: &[tooling::Token],
    interpolated: crate::syntax::Interpolated,
    names: usize,
    stop: parse::Stop<'_>,
) -> Option<usize> {
    let (mut payloads, mut rewritten, mut words, mut count) = (0, 0, 0, 0);
    for token in tokens {
        if stop() {
            return None;
        }
        match &token.kind {
            tooling::TokenKind::Words { entries, .. } => {
                count += entries.len();
                payloads += entries.capacity() * size_of::<Option<Vec<u8>>>();
                for entry in entries {
                    if stop() {
                        return None;
                    }
                    if let Some(entry) = entry {
                        payloads += entry.capacity();
                        rewritten += entry.len();
                    }
                }
            }
            tooling::TokenKind::Word => words += token.span.len(),
            _ => payloads += crate::typing::Heap::heap(token),
        }
    }
    Some(
        (tokens.len() + interpolated.tokens) * PER_TOKEN
            + (count + interpolated.entries) * PER_ENTRY
            + payloads
            + (words + interpolated.words) * WORD_COPIES
            + (rewritten + interpolated.rewritten) * REWRITE_COPIES
            + interpolated.bytes
            + names * NAME_COPIES,
    )
}

/// Adds the removed spellings in `source` to a static check's diagnostics,
/// in source order, charging a step for each of its tokens and each entry
/// of its percent literals, and for each of the `interpolated` tokens and
/// entries inside its interpolations, which it lexes again.
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
/// syntax error, if any, or `None` once the budget stops it. The text the
/// pass copies from the source and renders for its fixes may hold `room`
/// bytes beyond its footprint, or any amount when `None`; one that would
/// hold more stops the check, and what it held adds to the pass's
/// footprint.
pub(crate) fn add_to(
    checked: &mut crate::typing::Checked,
    source: &str,
    tokens: &[tooling::Token],
    interpolated: usize,
    within: &(dyn Fn(u64) -> bool + Sync),
    canonical: &dyn Fn(&str, u64) -> Option<Option<crate::Error>>,
    room: Option<usize>,
) {
    let Some(entries) = entries(tokens, &|| !within(checked.steps)) else {
        checked.stopped = true;
        return;
    };
    let read = u64::try_from(tokens.len() + interpolated + entries).unwrap_or(u64::MAX);
    checked.steps += read;
    // The room keeps the steps the pass charges, its sorts' with them, each
    // sort charged before it is made, once the budget is asked.
    let room = Room::new(room, within, checked.steps);
    let stop = || !room.within();
    let mut afford = || room.charge(read);
    // A compilation stopped since the check last asked reads nothing.
    if stop() {
        checked.stopped = true;
        return;
    }
    let walked = walk(source, tokens, &checked.calls, &mut afford, &stop, &room);
    checked.surface_bytes += room.peak();
    if room.full() {
        checked.stopped = true;
        return;
    }
    let Some(surface) = walked else {
        // The compiler's grammar reads the removed syntax only so that these
        // rules report it. A source they cannot read must parse without
        // it, so that removed syntax never compiles.
        let affordable = afford();
        checked.steps = room.total();
        if !affordable {
            checked.stopped = true;
            return;
        }
        let Some(error) = canonical(source, room.charged()) else {
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
    checked.steps = room.total();
    if surface.is_empty() {
        return;
    }
    // Leaving out the checker's diagnostics a removed spelling contains,
    // and putting the two lists in order, is a step for each diagnostic,
    // taken only within the budget.
    let merging = (checked.diagnostics.len() + surface.len()) as u64;
    let affordable = room.charge(merging);
    checked.steps = room.total();
    if !affordable {
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
    // Put in order, keeping those of a span in the order found, with the
    // copy the sort keeps for a moment taken from the room.
    let held = room.peak();
    let sorted = room.sort_by(&mut checked.diagnostics, |a, b| {
        (a.span.start, a.span.end).cmp(&(b.span.start, b.span.end))
    });
    checked.surface_bytes += room.peak() - held;
    checked.steps = room.total();
    if !sorted || !room.within() {
        checked.stopped = true;
    }
}

#[cfg(not(target_os = "wasi"))]
fn deep<'s>(
    source: &'s str,
    tokens: &[tooling::Token],
    calls: &CallTypes,
    stop: parse::Stop<'s>,
    room: &Room<'_>,
) -> Option<Vec<Diagnostic>> {
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(DEEP_STACK)
            .spawn_scoped(scope, || {
                parse::parse_tokens(source, tokens, usize::MAX, stop)
                    .ok()
                    .and_then(|tree| diagnostics(source, &tree, calls, stop, room))
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
    _: &Room<'_>,
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
    room: &Room<'_>,
) -> Option<Vec<Diagnostic>> {
    let mut checker = Checker {
        surface: Surface::new(source, tree, stop)?,
        calls,
        findings: Vec::new(),
        stop,
        visits: 0,
        stopped: false,
        room,
    };
    checker.found(source, tree, stop)
}

impl<'a> Checker<'a> {
    /// The removed spellings in `tree` as diagnostics, or `None` once the
    /// walk stops. The text each diagnostic copies takes from the room.
    fn found(
        &mut self,
        source: &str,
        tree: &'a syntax::Tree,
        stop: parse::Stop<'_>,
    ) -> Option<Vec<Diagnostic>> {
        self.program(&tree.body);
        if self.stopped || self.room.full() {
            return None;
        }
        let groups = self
            .surface
            .edits
            .groups(self.surface.rewrites.len(), stop)?;
        let mut diagnostics = Vec::new();
        for (group, rewrite) in self.surface.rewrites.iter().enumerate() {
            if group % POLL == 0 && stop() {
                return None;
            }
            let code = rewrite.rule.code();
            let edits: Vec<Edit> = self
                .surface
                .edits
                .flatten_group(source, &groups[group], self.room)?
                .into_iter()
                .map(|(at, replacement)| Edit {
                    span: span(at),
                    replacement,
                })
                .filter(|edit| edit.span.start < edit.span.end || !edit.replacement.is_empty())
                .collect();
            let mut message = Written::new(self.room);
            match rewrite.rule {
                Rule::Require => message.write(format_args!(
                    "`require` names modules and aliases with string literals, not `{}`; {}",
                    rewrite.removed, rewrite.advice
                )),
                _ => message.write(format_args!(
                    "`{}` was removed; {}",
                    rewrite.removed, rewrite.advice
                )),
            }
            let message = message.finish()?;
            if !self.room.take(rewrite.advice.len()) {
                return None;
            }
            let mut diagnostic = Diagnostic::error(code, span(rewrite.span), message);
            let fix = Fix::edits(rewrite.advice.clone(), edits);
            if !fix.edits.is_empty() && fix.applies(source) {
                diagnostic = diagnostic.with_fix(fix);
            }
            diagnostics.push(diagnostic);
        }
        for (index, finding) in self.findings.iter().enumerate() {
            if index % POLL == 0 && stop() {
                return None;
            }
            let code = finding.rule.code();
            let mut message = Written::new(self.room);
            message.write(format_args!(
                "`{}` was removed; {}",
                finding.removed, finding.advice
            ));
            let message = message.finish()?;
            let mut diagnostic = Diagnostic::error(code, span(finding.span), message);
            if !finding.suggestion.is_empty() {
                let mut edits = Vec::with_capacity(finding.suggestion.len());
                for (at, replacement) in &finding.suggestion {
                    if !self.room.take(replacement.len()) {
                        return None;
                    }
                    edits.push(Edit {
                        span: span(*at),
                        replacement: replacement.clone(),
                    });
                }
                if !self.room.take(finding.advice.len()) {
                    return None;
                }
                let fix = Fix::edits(finding.advice.clone(), edits).suggestion();
                if fix.applies(source) {
                    diagnostic = diagnostic.with_fix(fix);
                }
            }
            diagnostics.push(diagnostic);
        }
        if self.stopped || stop() {
            return None;
        }
        // Diagnostics of a span and code keep the order they were found in.
        let sorted = self.room.sort_by(&mut diagnostics, |a, b| {
            (a.span.start, a.span.end, a.code).cmp(&(b.span.start, b.span.end, b.code))
        });
        sorted.then_some(diagnostics)
    }

    /// `args` written out through the room, as `format!` writes them;
    /// empty once the room refuses them, which stops the walk.
    pub(super) fn written(&self, args: std::fmt::Arguments<'_>) -> String {
        let mut text = Written::new(self.room);
        text.write(args);
        // A refusal leaves the room full, which stops the walk.
        text.finish().unwrap_or_default()
    }

    /// A copy of `text`, the source's or the walk's, taken from the room
    /// as [`Self::written`] takes it.
    pub(super) fn copied(&self, text: &str) -> String {
        self.written(format_args!("{text}"))
    }

    /// Text written in pieces through the room, each taken as
    /// [`Self::written`] takes it.
    pub(super) fn writer(&self) -> Written<'a> {
        Written::new(self.room)
    }
}

/// How many statements, expressions or diagnostics the walk for removed
/// spellings takes between asking whether the compilation has stopped.
const POLL: usize = 1;

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
    /// Whether the walk gave up because the compilation stopped, or the
    /// text it copies outgrew its room.
    stopped: bool,
    /// What the text the walk copies and renders may hold.
    pub(super) room: &'a Room<'a>,
}

impl Checker<'_> {
    /// Counts a statement or an expression the walk visits, asking every
    /// [`POLL`] of them whether the compilation has stopped. Returns
    /// whether the walk should give up, as it does once its room is full.
    pub(super) fn halt(&mut self) -> bool {
        if self.room.full() {
            self.stopped = true;
        }
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
    /// A receiver kind the syntax decides: a literal, a builtin namespace,
    /// a rescued error or an annotated parameter.
    pub(super) fn static_kind(&self, receiver: &syntax::Expr) -> Option<String> {
        Some(
            match &receiver.kind {
                syntax::ExprKind::Str | syntax::ExprKind::Template(_) => "string",
                syntax::ExprKind::Symbol => "symbol",
                syntax::ExprKind::Array(_) | syntax::ExprKind::Words => "array",
                syntax::ExprKind::Hash(_) => "hash",
                syntax::ExprKind::Integer => "int",
                syntax::ExprKind::Float => "float",
                syntax::ExprKind::Range(..) => "range",
                syntax::ExprKind::Group(_, inner, _) => return self.static_kind(inner),
                syntax::ExprKind::Name(name) => {
                    if self.local(name) {
                        let scope = self.scope();
                        if scope.rescues.contains(name) {
                            return Some("error".to_owned());
                        }
                        let def = scope.def?;
                        let mut found = None;
                        for param in &def.params {
                            if !self.room.charge(1 + name.len().div_ceil(64) as u64) {
                                return None;
                            }
                            if param.name == *name {
                                found = Some(param);
                                break;
                            }
                        }
                        let param = found?;
                        let ty = param.ty.as_ref()?;
                        if ty.nullable {
                            return None;
                        }
                        let syntax::TypeKind::Named(tok, _) = &ty.kind else {
                            return None;
                        };
                        let written = self.token_text(*tok);
                        return [
                            "string", "symbol", "array", "hash", "int", "float", "money",
                            "duration", "time", "range",
                        ]
                        .into_iter()
                        .find(|kind| kind.eq_ignore_ascii_case(written))
                        .map(str::to_owned);
                    }
                    if super::context::namespace_name(name)
                        && !self.declared.classes.contains_key(name.as_str())
                    {
                        return Some(name.clone());
                    }
                    return None;
                }
                _ => return None,
            }
            .to_owned(),
        )
    }

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
                    name => written!(self, "class {name}"),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

    #[test]
    fn parameter_kind_lookup_obeys_its_work_budget() {
        let params = (0..256)
            .map(|i| format!("p{i}: string"))
            .collect::<Vec<_>>()
            .join(", ");
        let source = format!("def f({params}); p255.to_s(); end");
        let tree = parse::parse(&source).unwrap();
        let syntax::StmtKind::Def(def) = &tree.body[0].kind else {
            panic!("function")
        };
        let syntax::StmtKind::Expr(expr) = &def.body[0].kind else {
            panic!("expression")
        };
        let syntax::ExprKind::Call(call) = &expr.kind else {
            panic!("call")
        };
        let room = Room::new(None, &|steps| steps <= 8, 0);
        let mut checker = Checker {
            surface: Surface::new(&source, &tree, &|| false).unwrap(),
            calls: &CallTypes::default(),
            findings: Vec::new(),
            stop: &|| false,
            visits: 0,
            stopped: false,
            room: &room,
        };
        checker.scopes.push(super::super::context::Scope {
            def: Some(def),
            locals: ["p255".to_owned()].into_iter().collect(),
            ..Default::default()
        });
        assert!(
            checker
                .static_kind(call.receiver.as_ref().unwrap())
                .is_none()
        );
        assert!(!room.within());
    }

    #[test]
    fn footprint_stops_inside_one_percent_token() {
        let tokens = vec![tooling::Token {
            kind: tooling::TokenKind::Words {
                symbols: false,
                entries: vec![Some(vec![b'x']); 10_000],
            },
            span: 0..1,
            line: 1,
        }];
        let visits = AtomicUsize::new(0);
        assert!(
            footprint(
                &tokens,
                crate::syntax::Interpolated::default(),
                0,
                &|| visits.fetch_add(1, Relaxed) == 3
            )
            .is_none()
        );
        assert_eq!(visits.load(Relaxed), 4);
        assert!(entries(&tokens, &|| true).is_none());
    }
}
