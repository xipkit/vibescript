//! Repairs a migration with the static checker's diagnostics until it
//! type checks or no repair helps.
//!
//! Each round checks the source and proposes repairs for its errors:
//! rewrites that narrow a value where it is used, such as `fetch` for an
//! index the recorded runs never found missing, and then annotations the
//! migration wrote that widen to the types the checker found, such as a
//! result that a rescued or unrun branch returns. A repair is kept only when
//! it leaves fewer errors and, where the recorded invocations can run, when
//! every one of them still does exactly what it did before. The loop stops
//! at a fixpoint.
//!
//! The same probes infer types for code no recorded run reached: a result
//! or parameter the migration had to annotate `any` takes the types the
//! checker finds for the function's exits and for the arguments its callers
//! pass.

use super::{
    Code, Diagnostic as Note, Invocation, Migration, Observations,
    observe::{Facts, Outcome, replay},
    probe::{Request, probe},
    sites::{DefSite, defs},
    ty::Ty,
    types::Types,
};
use std::collections::{HashMap, HashSet};
use vibescript::{Engine, ModuleConfig, diagnostic::Diagnostic, surface::syntax::*};

/// The most rounds one source gets.
const ROUNDS: usize = 24;
/// The most candidate sources one repair checks.
const CHECKS: usize = 400;
/// The most times one repair runs the recorded invocations.
const RUNS: usize = 60;
/// The most widenings one repair tries repairing further from when they
/// leave as many errors as before.
const SPECULATIONS: usize = 4;

/// Repairs `migration` of `original` until the static checker accepts it
/// or no repair helps, keeping what `invocations` do.
///
/// Without invocations that can run, only repairs that cannot change what a
/// recorded run did are made: widening annotations the migration wrote, and
/// typing code that `observations` show no run reached.
///
/// ```
/// use vibescript_tools::migrate::{Invocation, Options, migrate, observe, repair};
/// let source = "def first(items)\n  items[0] + 1\nend\n";
/// let call = serde_json::json!({"function": "first", "args": [[1]]});
/// let runs = [Invocation::from_json(call, ".".as_ref())?];
/// let observations = observe(source, &runs);
/// let migration = migrate(source, &observations, &Options::default());
/// let repaired = repair(source, &migration, &runs, &observations);
/// assert_eq!(repaired.source, "def first(items: array<int>) -> int\n  items.fetch(0) + 1\nend\n");
/// # Ok::<(), String>(())
/// ```
pub fn repair(
    original: &str,
    migration: &Migration,
    invocations: &[Invocation],
    observations: &Observations,
) -> Migration {
    let mut engine = Engine::new();
    if let Some(paths) = invocations
        .iter()
        .map(Invocation::module_paths)
        .find(|paths| !paths.is_empty())
    {
        let _ = engine.set_module_config(ModuleConfig {
            paths,
            ..ModuleConfig::default()
        });
    }
    let runnable = !invocations.is_empty() && invocations.iter().all(Invocation::reproducible);
    let mut repairer = Repairer {
        engine,
        invocations: if runnable { invocations } else { &[] },
        repair: true,
        baseline: Vec::new(),
        facts: None,
        original: Origin::new(original, observations.facts(original)),
        checks: 0,
        runs: 0,
    };
    let mut repaired = migration.clone();
    if let Some(source) = repairer.run(&migration.source) {
        repaired.changed |= source != migration.source;
        repaired.source = source;
        repairer
            .original
            .settle_notes(&repaired.source, &mut repaired.diagnostics);
    }
    repaired
}

/// Types the results and parameters a migration of `original` had to
/// annotate `any` where no recorded run returned or passed a value, from
/// what the static checker finds for them, dropping the notes about those
/// it typed. Returns the new text, or none when nothing changed.
pub(crate) fn infer(
    original: &str,
    text: &str,
    facts: Option<&Facts>,
    notes: &mut Vec<Note>,
) -> Option<String> {
    let mut repairer = Repairer {
        engine: Engine::new(),
        invocations: &[],
        repair: false,
        baseline: Vec::new(),
        facts: None,
        original: Origin::new(original, facts),
        checks: 0,
        runs: 0,
    };
    let inferred = repairer.run(text)?;
    repairer.original.settle_notes(&inferred, notes);
    Some(inferred)
}

/// What the original source says about each function, to tell the
/// migration's annotations from the author's.
struct Origin {
    source: String,
    defs: Vec<OriginDef>,
}

/// Whether a function body yields.
fn yields(def: &Def) -> bool {
    let mut found = false;
    for stmt in super::sites::def_bodies(def) {
        super::sites::each_stmt(stmt, &mut |node| {
            if let super::sites::Node::Expr(Expr {
                kind: ExprKind::Yield(..),
                ..
            }) = node
            {
                found = true;
            }
            !found
        });
    }
    found
}

struct OriginDef {
    key: String,
    /// Whether the author declared the result.
    declared: bool,
    /// The parameters the author annotated.
    annotated: Vec<String>,
    /// Whether a recorded run entered the function, and returned from it,
    /// when runs were observed.
    entered: Option<bool>,
    returned: Option<Option<Types>>,
    /// Whether the function yields, so a block may break out of it with a
    /// value no run observed as its result.
    yields: bool,
    /// Where the migration's notes about the result and each parameter
    /// point.
    result_note: usize,
    param_notes: Vec<(usize, String)>,
}

impl Origin {
    fn new(source: &str, facts: Option<&Facts>) -> Self {
        let Ok(tree) = vibescript::surface::parse::parse(source) else {
            return Self {
                source: source.to_owned(),
                defs: Vec::new(),
            };
        };
        let defs = defs(&tree)
            .iter()
            .map(|site| {
                let def = site.def;
                let offset = tree.tokens[def.keyword].start;
                let result_note = match (&def.parens, def.params.last()) {
                    (Some((_, close)), _) => tree.tokens[*close].end,
                    (None, Some(last)) => last.span.end,
                    (None, None) => def.name_span.end,
                };
                OriginDef {
                    key: site.key(),
                    declared: def.result.is_some(),
                    annotated: def
                        .params
                        .iter()
                        .filter(|param| param.ty.is_some())
                        .map(|param| param.name.clone())
                        .collect(),
                    entered: facts.map(|facts| facts.starts.contains_key(&offset)),
                    returned: facts.map(|facts| facts.returns.get(&offset).cloned()),
                    yields: yields(def),
                    result_note,
                    param_notes: def
                        .params
                        .iter()
                        .map(|param| (tree.tokens[param.name_tok].end, param.name.clone()))
                        .collect(),
                }
            })
            .collect();
        Self {
            source: source.to_owned(),
            defs,
        }
    }

    /// Drops the notes about `any` results and parameters that the repair
    /// typed.
    fn settle_notes(&self, source: &str, notes: &mut Vec<Note>) {
        let Ok(tree) = vibescript::surface::parse::parse(source) else {
            return;
        };
        let sites = defs(&tree);
        let origin = self.pair(&sites);
        let vague =
            |ty: Option<&TypeExpr>| ty.is_none_or(|ty| source[ty.span.range()].contains("any"));
        let mut typed: HashSet<usize> = HashSet::new();
        for (site, origin) in sites.iter().zip(origin) {
            let Some(origin) = origin else {
                continue;
            };
            if !vague(site.def.result.as_ref().map(|(_, ty)| ty)) {
                typed.insert(origin.result_note);
            }
            for (offset, name) in &origin.param_notes {
                let param = site.def.params.iter().find(|param| param.name == *name);
                if param.is_some_and(|param| !vague(param.ty.as_ref())) {
                    typed.insert(*offset);
                }
            }
        }
        notes.retain(|note: &Note| {
            note.code != Code::Any
                || !typed.contains(&note.offset)
                || !(note.message.starts_with("the result of ")
                    || note.message.starts_with("parameter "))
        });
    }

    /// The original entry of each current function, when the functions
    /// correspond one to one.
    fn pair(&self, sites: &[DefSite<'_>]) -> Vec<Option<&OriginDef>> {
        let same = sites.len() == self.defs.len()
            && sites
                .iter()
                .zip(&self.defs)
                .all(|(site, def)| site.key() == def.key);
        if same {
            self.defs.iter().map(Some).collect()
        } else {
            sites.iter().map(|_| None).collect()
        }
    }
}

/// A set of edits to the current source, and how to judge it.
#[derive(Clone, Debug)]
pub(crate) struct Proposal {
    pub edits: Vec<(Span, String)>,
    /// Whether it may leave as many errors as before, because it types
    /// something `any` more precisely.
    pub tightens: bool,
    /// Whether it cannot change what a recorded run did.
    pub safe: bool,
}

struct Repairer<'a> {
    engine: Engine,
    invocations: &'a [Invocation],
    /// Whether to repair errors, or only to type what no run reached.
    repair: bool,
    /// What each invocation did before any repair.
    baseline: Vec<Outcome>,
    /// What the recorded runs observed of the current source.
    facts: Option<(String, Facts)>,
    original: Origin,
    checks: usize,
    runs: usize,
}

impl Repairer<'_> {
    /// The static errors in `text`, or none when it does not parse.
    fn errors(&mut self, text: &str) -> Option<Vec<Diagnostic>> {
        self.checks += 1;
        let checked = self.engine.type_check(text).ok()?;
        Some(
            checked
                .diagnostics
                .into_iter()
                .filter(|d| d.is_error() && d.file.is_none())
                .collect(),
        )
    }

    fn verifying(&self) -> bool {
        !self.invocations.is_empty()
    }

    /// Runs the loop from `text`, returning the repaired source, or none
    /// when nothing changed.
    fn run(&mut self, text: &str) -> Option<String> {
        let errors = self.errors(text)?;
        if self.verifying() {
            self.baseline = replay(text, self.invocations, false).ok()?.outcomes;
        }
        let (current, _) = self.settle(text.to_owned(), errors, true);
        (current != text).then_some(current)
    }

    /// Repairs `current` until no repair helps. Where `speculate`, it then
    /// tries each widening that left as many errors as before, since typing
    /// a value as what its callers pass can expose errors in code no run
    /// reached that further repairs fix, and keeps the first that ends with
    /// fewer errors.
    fn settle(
        &mut self,
        mut current: String,
        mut errors: Vec<Diagnostic>,
        speculate: bool,
    ) -> (String, Vec<Diagnostic>) {
        let mut speculations = 0;
        loop {
            for _ in 0..ROUNDS {
                let before = current.clone();
                if !errors.is_empty() && self.repair && self.verifying() {
                    let proposals = self.narrowings(&current, &errors);
                    if let Some((text, next)) = self.apply(&current, &errors, proposals) {
                        current = text;
                        errors = next;
                        continue;
                    }
                }
                if !errors.is_empty() && self.repair {
                    let proposals = self.widenings(&current, &errors);
                    let proposals = proposals
                        .into_iter()
                        .map(|proposal| vec![proposal])
                        .collect();
                    if let Some((text, next)) = self.apply(&current, &errors, proposals) {
                        current = text;
                        errors = next;
                        continue;
                    }
                }
                let proposals = self.tightenings(&current, &errors);
                let proposals = proposals
                    .into_iter()
                    .map(|proposal| vec![proposal])
                    .collect();
                if let Some((text, next)) = self.apply(&current, &errors, proposals) {
                    current = text;
                    errors = next;
                }
                if current == before || self.checks >= CHECKS {
                    break;
                }
            }
            if !speculate || !self.repair || errors.is_empty() || self.checks >= CHECKS {
                break;
            }
            let mut advanced = false;
            for proposal in self.widenings(&current, &errors) {
                if speculations >= SPECULATIONS || self.checks >= CHECKS {
                    break;
                }
                speculations += 1;
                let Some(candidate) = apply_edits(&current, &proposal.edits) else {
                    continue;
                };
                let Some(next) = self.errors(&candidate) else {
                    continue;
                };
                if !self.keeps_behaviour(&candidate, &[proposal]) {
                    continue;
                }
                let (text, after) = self.settle(candidate, next, false);
                if after.len() < errors.len() {
                    current = text;
                    errors = after;
                    advanced = true;
                    break;
                }
            }
            if !advanced {
                break;
            }
        }
        (current, errors)
    }

    /// Applies as many proposals as keep their promises: every group's
    /// first at once when they do together, and otherwise each group's in
    /// turn until one does.
    fn apply(
        &mut self,
        text: &str,
        errors: &[Diagnostic],
        groups: Vec<Vec<Proposal>>,
    ) -> Option<(String, Vec<Diagnostic>)> {
        // Errors with the same first repair, such as every use of one
        // local, are one group.
        let mut seen: Vec<Vec<(Span, String)>> = Vec::new();
        let groups: Vec<Vec<Proposal>> = groups
            .into_iter()
            .filter(|group| {
                let Some(first) = group.first() else {
                    return false;
                };
                let new = !seen.contains(&first.edits);
                seen.push(first.edits.clone());
                new
            })
            .collect();
        let firsts = disjoint(groups.iter().map(|group| group[0].clone()).collect());
        if firsts.is_empty() {
            return None;
        }
        if groups.len() > 1
            && let Some(accepted) = self.try_proposals(text, errors, &firsts)
        {
            return Some(accepted);
        }
        // Edits accepted so far, in the coordinates of `text`.
        let mut taken: Vec<(Span, String)> = Vec::new();
        let mut current: Option<(String, Vec<Diagnostic>)> = None;
        for group in &groups {
            for proposal in group {
                if self.checks >= CHECKS {
                    return current;
                }
                let overlaps = proposal.edits.iter().any(|(span, _)| {
                    taken
                        .iter()
                        .any(|(other, _)| !(span.end <= other.start || other.end <= span.start))
                });
                if overlaps {
                    continue;
                }
                let base = current.as_ref().map_or(text, |(text, _)| text.as_str());
                let base_errors = current.as_ref().map_or(errors, |(_, errors)| errors);
                let shifted = Proposal {
                    edits: proposal
                        .edits
                        .iter()
                        .map(|(span, text)| (shift(*span, &taken), text.clone()))
                        .collect(),
                    ..proposal.clone()
                };
                if let Some(accepted) = self.try_proposals(base, base_errors, &[shifted]) {
                    taken.extend(proposal.edits.iter().cloned());
                    current = Some(accepted);
                    break;
                }
            }
        }
        current
    }

    fn try_proposals(
        &mut self,
        text: &str,
        errors: &[Diagnostic],
        proposals: &[Proposal],
    ) -> Option<(String, Vec<Diagnostic>)> {
        let edits: Vec<(Span, String)> = proposals
            .iter()
            .flat_map(|proposal| proposal.edits.iter().cloned())
            .collect();
        let candidate = apply_edits(text, &edits)?;
        let next = self.errors(&candidate)?;
        let tightens = proposals.iter().all(|proposal| proposal.tightens);
        let improves = next.len() < errors.len() || (tightens && next.len() <= errors.len());
        if !improves || !self.keeps_behaviour(&candidate, proposals) {
            return None;
        }
        Some((candidate, next))
    }

    /// Whether `candidate` compiles and does what the source did: every
    /// recorded invocation has the same outcome, or, where none can run,
    /// every proposal that made it is safe.
    fn keeps_behaviour(&mut self, candidate: &str, proposals: &[Proposal]) -> bool {
        if vibescript::Engine::new().compile(candidate).is_err() {
            return false;
        }
        if !self.verifying() {
            return proposals.iter().all(|proposal| proposal.safe);
        }
        if self.runs >= RUNS {
            return false;
        }
        self.runs += 1;
        let Ok(replayed) = replay(candidate, self.invocations, false) else {
            return false;
        };
        replayed.outcomes.len() == self.baseline.len()
            && replayed
                .outcomes
                .iter()
                .zip(&self.baseline)
                .all(|(new, old)| new.same(old))
    }

    /// What the recorded runs observe of `text`, when they can run.
    fn facts_of(&mut self, text: &str) -> Option<&Facts> {
        if !self.verifying() {
            return None;
        }
        if self.facts.as_ref().is_none_or(|(own, _)| own != text) {
            self.runs += 1;
            let facts = replay(text, self.invocations, true).ok()?.facts?;
            self.facts = Some((text.to_owned(), facts));
        }
        self.facts.as_ref().map(|(_, facts)| facts)
    }

    /// Narrows values where they are used, from what the recorded runs of
    /// `text` observed.
    fn narrowings(&mut self, text: &str, errors: &[Diagnostic]) -> Vec<Vec<Proposal>> {
        let Ok(tree) = vibescript::surface::parse::parse(text) else {
            return Vec::new();
        };
        let facts = self.facts_of(text);
        super::narrow::narrowings(text, &tree, errors, facts)
    }

    /// Widens the results the migration annotated to what their exits
    /// return, for functions with an error inside.
    fn widenings(&mut self, text: &str, errors: &[Diagnostic]) -> Vec<Proposal> {
        let Ok(tree) = vibescript::surface::parse::parse(text) else {
            return Vec::new();
        };
        let sites = defs(&tree);
        let origin = self.original.pair(&sites);
        let mut request = Request::default();
        for (index, site) in sites.iter().enumerate() {
            let written = origin[index].is_some_and(|origin| !origin.declared);
            let erring = errors.iter().any(|error| {
                site.span.start <= error.span.start && error.span.start < site.span.end
            });
            if written && erring && site.def.result.is_some() {
                request.results.push(index);
            }
        }
        let written_param = |index: usize, name: &str| {
            origin[index].is_some_and(|origin| !origin.annotated.iter().any(|p| p == name))
        };
        let original = &self.original.source;
        let authored =
            |name: &str, annotation: &str| original.contains(&format!("{name}: {annotation}"));
        let mut proposals =
            super::widen::widenings(text, &tree, &sites, errors, &written_param, &authored);
        if request.results.is_empty() {
            return proposals;
        }
        let found = probe(&self.engine, text, &tree, &request);
        for index in request.results {
            let Some(exits) = found.results.get(&index) else {
                continue;
            };
            let site = &sites[index];
            let (_, annotation) = site.def.result.as_ref().unwrap();
            let written = &text[annotation.span.range()];
            let Some(current) = Ty::parse(written) else {
                continue;
            };
            let concrete: Vec<Ty> = exits
                .iter()
                .filter(|ty| **ty != Ty::Any || current == Ty::Any)
                .cloned()
                .collect();
            let widened = current.clone().join(Ty::union(concrete));
            let Some(rendered) = widened.render() else {
                continue;
            };
            if rendered != written && widened != current {
                proposals.push(Proposal {
                    edits: vec![(annotation.span, rendered)],
                    tightens: false,
                    safe: true,
                });
            }
        }
        proposals
    }

    /// Types more precisely what an annotation leaves `any`: a result the
    /// migration annotated with `any` in it takes what the runs returned and
    /// what the checker finds its exits return; a parameter it annotated
    /// `any` in a function no run entered takes what its callers pass; and,
    /// where the runs can confirm it, an author's bare `hash` or `array`
    /// becomes a dictionary or array of the types it held.
    fn tightenings(&mut self, text: &str, errors: &[Diagnostic]) -> Vec<Proposal> {
        let Ok(tree) = vibescript::surface::parse::parse(text) else {
            return Vec::new();
        };
        let sites = defs(&tree);
        let vague = |ty: &TypeExpr| Ty::parse(&text[ty.span.range()]).is_none_or(|ty| ty.vague());
        let bare = |ty: &TypeExpr| matches!(&text[ty.span.range()], "hash" | "array");
        let candidates = sites.iter().any(|site| {
            site.def.result.as_ref().is_some_and(|(_, ty)| vague(ty))
                || site
                    .def
                    .params
                    .iter()
                    .any(|param| param.ty.as_ref().is_some_and(vague))
        });
        if !candidates {
            return Vec::new();
        }
        let verifying = self.verifying();
        let facts = self.facts_of(text);
        // What the runs of this text returned from each function, and
        // which functions they entered.
        let observed = facts.map(|facts| {
            let returns: HashMap<usize, Types> = facts.returns.clone();
            let starts: HashSet<usize> = facts.starts.keys().copied().collect();
            let params: Vec<(usize, String, Types)> = facts
                .params
                .iter()
                .map(|(offset, name, types)| (*offset, name.to_owned(), types.clone()))
                .collect();
            (returns, starts, params)
        });
        let origin = self.original.pair(&sites);
        let surface = vibescript::surface::Surface::new(text, &tree);
        let known = |name: &str| surface.known_type(name);
        let mut request = Request::default();
        // The floor each probed result keeps: what the runs returned.
        let mut floors: HashMap<usize, Ty> = HashMap::new();
        let mut bare_results: HashSet<usize> = HashSet::new();
        let mut proposals = Vec::new();
        for (index, site) in sites.iter().enumerate() {
            let Some(origin) = origin[index] else {
                continue;
            };
            let offset = tree.tokens[site.def.keyword].start;
            let (returned, entered) = match &observed {
                Some((returns, starts, _)) => (
                    Some(returns.get(&offset).cloned()),
                    Some(starts.contains(&offset)),
                ),
                None => (
                    origin
                        .returned
                        .as_ref()
                        .map(|types| types.clone().filter(|t| !t.is_empty())),
                    origin.entered,
                ),
            };
            // An exit whose value has an error has no type to report, so
            // where nothing says what the function returned, every exit must
            // be typed.
            let clean = !errors.iter().any(|error| {
                site.span.start <= error.span.start && error.span.start < site.span.end
            });
            // A block may have broken out of a function that ran and
            // yields, with a value no run saw returned.
            let broken = entered == Some(true) && origin.yields && !verifying;
            if let Some((_, ty)) = &site.def.result
                && !broken
                && (returned.is_some() || clean)
                && (!origin.declared && vague(ty) || verifying && origin.declared && bare(ty))
            {
                let floor = returned
                    .flatten()
                    .map_or(Ty::Never, |types| Ty::observed(&types, &known));
                floors.insert(index, floor);
                if origin.declared {
                    bare_results.insert(index);
                }
                request.results.push(index);
            }
            for param in &site.def.params {
                let Some(ty) = &param.ty else {
                    continue;
                };
                // An author's bare collection, from what the runs passed.
                if verifying && origin.annotated.contains(&param.name) && bare(ty) {
                    let seen = observed.as_ref().and_then(|(_, _, params)| {
                        params
                            .iter()
                            .find(|(at, name, _)| *at == offset && *name == param.name)
                            .map(|(_, _, types)| Ty::observed(types, &known))
                    });
                    if let Some(rendered) =
                        seen.and_then(|seen| spelled(seen, &text[ty.span.range()] == "hash"))
                    {
                        proposals.push(Proposal {
                            edits: vec![(ty.span, rendered)],
                            tightens: true,
                            safe: false,
                        });
                    }
                    continue;
                }
                // Arguments may come from a host calling the function
                // directly, which only a run can rule out.
                if entered == Some(false)
                    && !origin.annotated.contains(&param.name)
                    && &text[ty.span.range()] == "any"
                    && matches!(param.kind, ParamKind::Positional | ParamKind::Keyword)
                {
                    request.params.push((index, param.name.clone()));
                }
            }
        }
        if request.results.is_empty() && request.params.is_empty() {
            return proposals;
        }
        let found = probe(&self.engine, text, &tree, &request);
        for index in &request.results {
            let Some(exits) = found.results.get(index) else {
                continue;
            };
            let (_, annotation) = sites[*index].def.result.as_ref().unwrap();
            let floor = floors.remove(index).unwrap_or(Ty::Never);
            let ty = Ty::union(exits.iter().cloned().chain([floor]));
            let rendered = if bare_results.contains(index) {
                spelled(ty, &text[annotation.span.range()] == "hash")
            } else if ty.vague() || ty == Ty::Nil || ty == Ty::Never {
                None
            } else {
                ty.render()
            };
            if let Some(rendered) = rendered
                && rendered != text[annotation.span.range()]
            {
                proposals.push(Proposal {
                    edits: vec![(annotation.span, rendered)],
                    tightens: true,
                    safe: !bare_results.contains(index),
                });
            }
        }
        for (index, name) in &request.params {
            let Some(arguments) = found.params.get(&(*index, name.clone())) else {
                continue;
            };
            let ty = Ty::union(arguments.iter().cloned());
            if ty.vague() || ty == Ty::Nil {
                continue;
            }
            let param = sites[*index]
                .def
                .params
                .iter()
                .find(|param| param.name == *name)
                .unwrap();
            let annotation = param.ty.as_ref().unwrap();
            if let Some(rendered) = ty.render() {
                proposals.push(Proposal {
                    edits: vec![(annotation.span, rendered)],
                    tightens: true,
                    safe: true,
                });
            }
        }
        proposals
    }
}

/// A type as an author's bare `hash` or `array` meant it: a dictionary of
/// what a record's fields held, or an array; none when it is still vague.
fn spelled(ty: Ty, hash: bool) -> Option<String> {
    let ty = match ty {
        Ty::Shape(fields, _) if hash => {
            Ty::Hash(Box::new(Ty::union(fields.into_iter().map(|f| f.ty))))
        }
        ty @ Ty::Hash(_) if hash => ty,
        ty @ Ty::Array(_) if !hash => ty,
        Ty::Tuple(items) if !hash => Ty::Array(Box::new(Ty::union(items))),
        _ => return None,
    };
    if ty.vague() { None } else { ty.render() }
}

/// The proposals whose edits overlap no earlier one's.
fn disjoint(proposals: Vec<Proposal>) -> Vec<Proposal> {
    let mut taken: Vec<Span> = Vec::new();
    let mut kept = Vec::new();
    for proposal in proposals {
        let clear = proposal.edits.iter().all(|(span, _)| {
            taken
                .iter()
                .all(|other| span.end <= other.start || other.end <= span.start)
        });
        if clear {
            taken.extend(proposal.edits.iter().map(|(span, _)| *span));
            kept.push(proposal);
        }
    }
    kept
}

/// Where `span` of a text lies once `edits` of it, which it does not
/// overlap, are applied.
fn shift(span: Span, edits: &[(Span, String)]) -> Span {
    let delta: isize = edits
        .iter()
        .filter(|(edit, _)| edit.end <= span.start)
        .map(|(edit, text)| text.len() as isize - (edit.end - edit.start) as isize)
        .sum();
    Span {
        start: (span.start as isize + delta) as usize,
        end: (span.end as isize + delta) as usize,
    }
}

fn apply_edits(text: &str, edits: &[(Span, String)]) -> Option<String> {
    let mut edits: Vec<&(Span, String)> = edits.iter().collect();
    edits.sort_by_key(|(span, _)| (span.start, span.end));
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0;
    for (span, replacement) in edits {
        if span.start < cursor || span.end > text.len() {
            return None;
        }
        out.push_str(text.get(cursor..span.start)?);
        out.push_str(replacement);
        cursor = span.end;
    }
    out.push_str(&text[cursor..]);
    Some(out)
}
