//! Checks function bodies: statements, locals, flow and narrowing.

use super::{
    Checker,
    counted::{CountedMap, CountedVec, ScratchVec},
    flow::{Branch, Flow, LocalId, Mark, VarState},
    meter::Heap,
    program::{FnId, NsId},
    sigs::BlockSig,
    ty::{Kind, Ty},
};
use crate::{
    diagnostic::{Code, Diagnostic, Edit, Fix, Span},
    syntax::{Expr, Node, Statement, Stmt, Target},
};
use std::collections::HashMap;

/// Why a symbol literal passed to a builtin stays a symbol.
pub(super) const BUILTIN_SYMBOL: &str = "a builtin receives it as a symbol";
/// Why a symbol literal written through an index stays a symbol.
pub(super) const INDEX_SYMBOL: &str = "a write through an index stores it as a symbol";
/// Why a symbol literal assigned to a class variable stays a symbol.
pub(super) const CLASS_SYMBOL: &str = "a class variable stores it as a symbol";
/// Why a symbol literal assigned to a parameter or a local without a
/// declared type stays a symbol.
pub(super) const LOCAL_SYMBOL: &str =
    "assigning a parameter, or a local without a declared type, stores it as a symbol";

/// A call of a required file's own code in its body, with the top-level
/// locals assigned where it runs.
pub(crate) struct FileCall {
    pub callee: FnId,
    pub span: Span,
    pub assigned: Vec<String>,
}

/// A local variable or parameter.
pub(crate) struct Local {
    pub name: String,
    pub declared: Ty,
    /// Where it was declared, for "declared here" notes and fixes.
    pub offset: usize,
    /// Whether an annotation fixed its type.
    pub annotated: bool,
    /// Whether the runtime checks each value assigned to it: a local
    /// declared with a type, but not a parameter.
    pub checked: bool,
    /// The declaring hash literal's field types, when a fix may declare the
    /// local as a dictionary.
    pub dictionary: Option<Ty>,
}

impl super::counted::Owned for Local {
    fn owned(&self) -> usize {
        self.name.capacity()
    }
}

impl super::counted::Owned for FileCall {
    fn owned(&self) -> usize {
        super::counted::Owned::owned(&self.assigned)
    }
}

/// The name of the function a `break` returns through; its exits and
/// results are counted as they grow.
impl super::counted::Owned for Context {
    fn owned(&self) -> usize {
        match self {
            Context::Block {
                break_to: Some(to), ..
            } => to.function.capacity(),
            _ => 0,
        }
    }
}

impl super::counted::Owned for Purpose {
    fn owned(&self) -> usize {
        self.heap()
    }
}

/// The states a loop or block is left or continued with.
#[derive(Default)]
pub(crate) struct Exits {
    /// The state at each `break`, which leaves the loop or the call.
    pub breaks: CountedVec<Branch>,
    /// The state at each `next`, which starts the next iteration.
    pub nexts: CountedVec<Branch>,
    /// The types of the values `break` gives.
    pub values: CountedVec<Ty>,
}

/// The type a `break` value out of a script function's block must have:
/// the function's result, which it returns through, or when the function
/// yields `inside` a loop or a block, its block's result, which ends that
/// loop or block.
#[derive(Clone)]
pub(crate) struct BreakTo {
    pub ty: Ty,
    pub function: String,
    pub inside: bool,
}

/// An enclosing construct that `next` and `break` refer to.
pub(crate) enum Context {
    Loop {
        mark: Mark,
        exits: Exits,
    },
    Block {
        mark: Mark,
        exits: Exits,
        /// The type the block must return, when known.
        result: Option<Ty>,
        /// Context for literals while inferring a generic block result.
        hint: Option<Ty>,
        /// Where a `break` value goes when a script function is called with
        /// the block, and the type it must have.
        break_to: Option<BreakTo>,
        /// Whether the block's value is used at all.
        used: bool,
        /// The values `next` and the tail gave, for inference.
        results: CountedVec<Ty>,
    },
}

impl Context {
    pub fn exits(&mut self) -> &mut Exits {
        match self {
            Context::Loop { exits, .. } | Context::Block { exits, .. } => exits,
        }
    }

    fn mark(&self) -> Mark {
        match self {
            Context::Loop { mark, .. } | Context::Block { mark, .. } => *mark,
        }
    }
}

/// The state of the function being checked.
pub(crate) struct Frame {
    pub owner: Option<NsId>,
    /// Whether `self` is an instance.
    pub instance: bool,
    /// The declared result; `None` for a function that returns `nil`.
    pub result: Option<Ty>,
    pub main: bool,
    pub name: String,
    /// The function's own typed block parameter.
    pub block: Option<BlockSig>,
    /// A pseudo-local that is assigned where `block_given?` holds.
    pub block_given: Option<LocalId>,
    /// In `initialize`: the instance variables it must assign, in name
    /// order, and the first of the pseudo-locals that follow each's
    /// assignment in the same order.
    pub initialize: Option<(super::construction::Roster, LocalId)>,
    /// The function being checked.
    pub function: Option<FnId>,
    /// In an instance variable's default: the variables not assigned yet.
    pub building: Option<super::construction::Unassigned>,
    pub locals: CountedVec<Local>,
    pub names: CountedMap<String, LocalId>,
    pub ambient: CountedVec<LocalId>,
    /// Names each open block scope shadowed, to restore when it closes.
    pub scopes: CountedVec<CountedVec<(String, Option<LocalId>)>>,
    pub flow: Flow,
    pub contexts: CountedVec<Context>,
    /// Whether the body is a class or module body, whose capitalized
    /// assignments are constants.
    pub namespace_body: bool,
    /// In a required file's function or method: the locals that are the
    /// file's top-level locals.
    pub shared: CountedVec<LocalId>,
    /// What the locals' names take, in the locals, the map of them by name
    /// and the scopes that record them.
    pub name_bytes: usize,
}

impl Frame {
    pub fn new(
        meter: &std::sync::Arc<super::meter::Meter>,
        owner: Option<NsId>,
        instance: bool,
        result: Option<Ty>,
        name: String,
    ) -> Self {
        Self {
            owner,
            instance,
            result,
            main: false,
            name,
            block: None,
            block_given: None,
            initialize: None,
            function: None,
            building: None,
            locals: CountedVec::new(),
            names: CountedMap::new(),
            ambient: CountedVec::new(),
            scopes: CountedVec::new(),
            flow: Flow::new(std::sync::Arc::clone(meter)),
            contexts: CountedVec::new(),
            namespace_body: false,
            shared: CountedVec::new(),
            name_bytes: 0,
        }
    }
}

impl Heap for Frame {
    fn heap(&self) -> usize {
        use super::meter::{map, vec};
        vec(self.locals.as_vec())
            + map(&self.names)
            + self.name_bytes
            + vec(self.scopes.as_vec())
            + self
                .scopes
                .iter()
                .map(|scope| vec(scope.as_vec()))
                .sum::<usize>()
            + self.flow.bytes()
            + vec(self.contexts.as_vec())
            + self
                .initialize
                .as_ref()
                .map_or(0, |(roster, _)| super::construction::roster_bytes(roster))
            + vec(self.ambient.as_vec())
            + vec(self.shared.as_vec())
            + self.name.heap()
    }
}

impl Heap for Context {
    fn heap(&self) -> usize {
        match self {
            Context::Loop { exits, .. } => exits.heap(),
            Context::Block {
                exits,
                results,
                break_to,
                ..
            } => {
                exits.heap()
                    + super::meter::vec(results.as_vec())
                    + break_to.as_ref().map_or(0, |to| to.function.heap())
            }
        }
    }
}

impl Heap for Exits {
    fn heap(&self) -> usize {
        self.breaks.heap() + self.nexts.heap() + super::meter::vec(self.values.as_vec())
    }
}

/// What a statement's value is for.
#[derive(Clone, Copy)]
pub(crate) enum Want {
    /// Its value is discarded.
    Discard,
    /// Its value is used; the type, when given, is context for literals.
    Infer(Option<Ty>),
    /// Its value must be assignable to this type.
    Check(Ty),
}

impl Want {
    pub fn hint(self) -> Option<Ty> {
        match self {
            Want::Discard => None,
            Want::Infer(hint) => hint,
            Want::Check(ty) => Some(ty),
        }
    }
}

/// Narrowings a condition implies for locals, when it holds and when not,
/// each in a list counted while it lives, so a condition composed of many
/// is counted at every level as its lists grow.
pub(crate) struct Narrow {
    pub then: ScratchVec<(LocalId, Ty)>,
    pub otherwise: ScratchVec<(LocalId, Ty)>,
}

impl Narrow {
    /// None, in lists that count with `meter`.
    pub fn new(meter: &std::sync::Arc<super::meter::Meter>) -> Self {
        Self {
            then: ScratchVec::new(meter),
            otherwise: ScratchVec::new(meter),
        }
    }
}

impl<'a> Checker<'a> {
    pub(super) fn report(&mut self, diagnostic: Diagnostic) {
        if self.mute > 0 {
            return;
        }
        // A check its budget stops keeps no more findings, nor one it
        // refuses room for; one it keeps is counted, with what it owns, as
        // it is kept, and by the measures after.
        let bytes = diagnostic.heap();
        if self
            .diagnostics
            .push(self.meter.tables(), diagnostic)
            .is_ok()
        {
            self.grown += bytes;
        } else {
            self.stopped = true;
        }
    }

    /// Refuses syntax taller than [`super::HEIGHT`], which the checker does
    /// not descend into.
    pub(super) fn too_deep(&mut self, span: Span) {
        self.too_deep = true;
        self.report(Diagnostic::error(
            Code::SYNTAX,
            span,
            text!(
                self,
                "syntax nesting too deep to type check: on WASI the checker allows {} levels",
                super::HEIGHT
            ),
        ));
    }

    /// Checks every function and namespace body.
    pub(super) fn check_all(&mut self) {
        if self.program.file {
            // The file's own locals, which its functions and methods share,
            // in a list counted while it lives.
            let mut shared = ScratchVec::new(&self.meter);
            for decl in self.program.fns.iter().filter(|decl| decl.main) {
                if let Some(def) = decl.def {
                    let scratch = assigned_names(&self.meter, &def.body, &mut shared);
                    if self.transient(scratch) {
                        return;
                    }
                }
            }
            // The names move into a set, counted before it is made.
            if self.transient(super::meter::table::<String>(shared.len())) {
                return;
            }
            let mut shared: std::collections::HashSet<String> =
                shared.into_vec().into_iter().collect();
            // The names the functions assign, and each function's, in lists
            // counted while they live.
            let mut written = ScratchVec::new(&self.meter);
            for decl in self.program.fns.iter().filter(|decl| !decl.main) {
                if self.halted() {
                    return;
                }
                let Some(def) = decl.def else { continue };
                let mut names = ScratchVec::new(&self.meter);
                let scratch = assigned_names(&self.meter, &def.body, &mut names);
                if self.transient(scratch + shared.heap()) {
                    return;
                }
                // Its parameters are its own, not the file's: the names
                // they take are set aside while its names are kept, in a
                // list counted while it lives, so each name is one search.
                let mut aside = ScratchVec::new(&self.meter);
                for param in def.params.iter() {
                    if let Some(name) = shared.take(param.name.as_str()) {
                        aside.add(name);
                    }
                }
                if written.reserve(names.len()).is_err() {
                    return;
                }
                for name in names.into_vec() {
                    if shared.contains(&name) {
                        written.add(name);
                    }
                }
                // The parameters' names are the file's again once its names
                // are kept.
                shared.extend(aside.into_vec());
            }
            // The names move into the program's set, which, with them, is
            // counted before it is made.
            let declarations = self.meter.declarations();
            let names: usize = written.iter().map(String::capacity).sum();
            let Ok(mut kept) = declarations.keep(names) else {
                return;
            };
            if self
                .program
                .file_written
                .reserve(declarations, written.len())
                .is_err()
            {
                return;
            }
            for name in written.into_vec() {
                self.program.file_written.insert_kept(&mut kept, name);
            }
            if self.declared() {
                return;
            }
        }
        if let Some(main) = self.program.fns.iter().position(|decl| decl.main) {
            self.check_function(main);
        }
        // A check past its budget checks no more namespaces or functions:
        // setting up each would still take work, such as declaring a
        // required file's locals in every function.
        for ns in 0..self.program.namespaces.len() {
            if self.over_budget() {
                return;
            }
            self.check_namespace_body(ns as NsId);
        }
        for id in 0..self.program.fns.len() {
            if self.over_budget() {
                return;
            }
            if !self.program.fns[id].main {
                self.check_function(id);
            }
        }
        if self.over_budget() {
            return;
        }
        self.check_file_calls();
        self.finish_construction();
    }

    /// Checks the body of namespace `ns` once, after those of the
    /// namespaces nested in it. The walk keeps its place on the heap, since
    /// namespaces nest as deep as the parser allows.
    fn check_namespace_body(&mut self, ns: NsId) {
        // The namespaces left at each level of nesting, in lists counted
        // while they live.
        let mut pending = ScratchVec::new(&self.meter);
        self.enter_namespace(ns, &mut pending);
        while let Some((ns, children)) = pending.last_mut() {
            let ns = *ns;
            match children.pop() {
                Some(child) => self.enter_namespace(child, &mut pending),
                None => {
                    pending.pop();
                    self.namespace_body(ns);
                }
            }
        }
    }

    /// Marks `ns` checked and queues it with its children, unless it was
    /// checked already.
    fn enter_namespace(&mut self, ns: NsId, pending: &mut ScratchVec<(NsId, ScratchVec<NsId>)>) {
        let namespace = &mut self.program.namespaces[ns as usize];
        if namespace.checked {
            return;
        }
        namespace.checked = true;
        let mut children = ScratchVec::new(&self.meter);
        if children.reserve(namespace.children.len()).is_err() {
            return;
        }
        for &child in namespace.children.values() {
            children.add(child);
        }
        // Popped from the end, so checked in ascending order; a check the
        // sort stops checks none of them.
        if super::counted::sort_unstable_by(&self.meter, &mut children, |a, b| b.cmp(a)).is_err() {
            return;
        }
        let tallest = namespace
            .module
            .filter(|module| namespace.parent.is_none() && super::too_tall(module.height()));
        if let Some(module) = tallest {
            let span = self.spans.token(module.offset as usize);
            self.too_deep(span);
        }
        pending.add((ns, children));
    }

    /// The instance-variable defaults of the class at `offset`, in order,
    /// with the variable each assigns. The first call gathers every
    /// class's in one pass over the declarations, rather than each class
    /// scanning all of them.
    fn defaults_of(&mut self, offset: u32) -> Vec<(&'a Stmt, Option<&'a str>)> {
        if self.defaults.is_none() {
            let additions = &self.parsed.additions;
            // Both indexes are counted before they are built: the names by
            // class and offset, and each class's defaults, which take at
            // most four places a class, or twice their number.
            let (ivars, count) = (additions.ivars.len(), additions.defaults.len());
            let most = super::meter::table::<((u32, u32), &str)>(ivars)
                + super::meter::table::<(u32, Vec<(&Stmt, Option<&str>)>)>(count)
                + 4 * count * std::mem::size_of::<(&Stmt, Option<&str>)>();
            if self.transient(most) {
                return Vec::new();
            }
            let mut names: HashMap<(u32, u32), &'a str> = HashMap::with_capacity(ivars);
            let mut defaults: HashMap<u32, Vec<(&'a Stmt, Option<&'a str>)>> = HashMap::new();
            // Each entry is a step, and the budget is checked as a walk
            // checks it.
            let pace = super::walk::PACE as usize;
            for (index, (class, ivar)) in additions.ivars.iter().enumerate() {
                if index % pace == pace - 1 && self.meter.pace(pace as u64, most) {
                    return Vec::new();
                }
                names.insert((*class, ivar.offset), ivar.name.as_str());
            }
            for (index, (class, stmt)) in additions.defaults.iter().enumerate() {
                if index % pace == pace - 1 && self.meter.pace(pace as u64, most) {
                    return Vec::new();
                }
                let name = names.get(&(*class, stmt.offset)).copied();
                defaults.entry(*class).or_default().push((stmt, name));
            }
            if self.meter.charge(((ivars % pace) + (count % pace)) as u64) {
                return Vec::new();
            }
            let bytes = super::meter::map(&defaults)
                + defaults.values().map(super::meter::vec).sum::<usize>();
            // Kept for the rest of the check.
            if self.hold(bytes).is_none() {
                return Vec::new();
            }
            self.defaults = Some((defaults, bytes));
        }
        let (defaults, bytes) = self.defaults.take().unwrap();
        let mut defaults = defaults;
        let taken = defaults.remove(&offset).unwrap_or_default();
        self.defaults = Some((defaults, bytes));
        taken
    }

    fn namespace_body(&mut self, ns: NsId) {
        let Some(module) = self.program.namespaces[ns as usize].module else {
            return;
        };
        // The frame keeps a copy of the namespace's name, counted before it
        // is made.
        let name = &self.program.namespaces[ns as usize].name;
        if self.meter.tables().keep(name.len()).is_err() {
            return;
        }
        let name = name.clone();
        let mut frame = Frame::new(&self.meter, Some(ns), false, None, name);
        frame.namespace_body = true;
        let previous = self.enter_frame(frame);
        // Only the enclosing locals the body names can matter to it, and
        // copying every one into each of many namespaces would take their
        // number times the namespaces.
        let mut mentioned = super::counted::ScratchSet::new(&self.meter);
        let scratch = mentions(&self.meter, &module.body, &mut mentioned);
        if self.transient(scratch) {
            self.leave_frame(previous);
            return;
        }
        // The enclosing locals the body names, with a copy of each name,
        // counted before they are copied; a check past its budget copies
        // none.
        let named = || {
            mentioned
                .iter()
                .filter_map(|&name| previous.names.get(name).map(|&id| (name, id)))
        };
        let (count, bytes) = named().fold((0, 0), |(count, bytes), (name, _)| {
            (count + 1, bytes + name.len())
        });
        let Some(held) =
            self.hold(count * std::mem::size_of::<(String, Ty, usize, VarState)>() + bytes)
        else {
            self.leave_frame(previous);
            return;
        };
        let mut ambient = Vec::with_capacity(count);
        ambient.extend(named().map(|(name, id)| {
            let local = &previous.locals[id as usize];
            (
                name.to_owned(),
                local.declared,
                local.offset,
                previous.flow.get(id),
            )
        }));
        for (name, declared, offset, state) in &ambient {
            // A check the budget stops declares no more of them, and the
            // body it checks next reads no code.
            let Some(id) = self.declare(name, *declared, *offset, true) else {
                break;
            };
            self.frame.flow.set(id, *state);
            // Declared in turn, so listed in the order of their ids.
            debug_assert!(self.frame.ambient.last().is_none_or(|&last| last < id));
            if self.frame.ambient.push(self.meter.tables(), id).is_err() {
                break;
            }
        }
        self.stmts(&module.body, Want::Discard);
        self.release(held);
        // What the body left of them, counted before it is copied.
        let (count, bytes) = ambient
            .iter()
            .filter(|(name, ..)| self.local(name).is_some())
            .fold((0, 0), |(count, bytes), (name, ..)| {
                (count + 1, bytes + name.len())
            });
        if self.transient(count * std::mem::size_of::<(String, VarState)>() + bytes) {
            self.leave_frame(previous);
            return;
        }
        let mut changes = Vec::with_capacity(count);
        changes.extend(ambient.iter().filter_map(|(name, _, _, _)| {
            self.local(name)
                .map(|id| (name.clone(), self.frame.flow.get(id)))
        }));
        // Instance-variable defaults run for each instance.
        let defaults = self.defaults_of(module.offset);
        if self.meter.tables().keep(self.frame.name.len()).is_err() {
            self.leave_frame(previous);
            return;
        }
        let name = self.frame.name.clone();
        let body = self.enter_frame(Frame::new(&self.meter, Some(ns), true, None, name));
        // A default may read only the variables whose defaults precede it:
        // those that must be assigned, whose roster, and the list it is
        // made from, are held before the names are copied.
        let ivars = &self.program.namespaces[ns as usize].ivars;
        let (mut count, mut bytes) = (0, 0);
        for (name, ivar) in ivars {
            if !self.types.assignable(Ty::NIL, ivar.ty) {
                count += 1;
                bytes += name.len();
            }
        }
        let Some(held) = self.hold(2 * count * std::mem::size_of::<String>() + bytes) else {
            self.leave_frame(body);
            self.leave_frame(previous);
            return;
        };
        let mut unassigned: Vec<String> = Vec::with_capacity(count);
        for (name, ivar) in &self.program.namespaces[ns as usize].ivars {
            // A check that stops takes every type as assignable, so lists
            // no more of them than it counted.
            if !self.types.assignable(Ty::NIL, ivar.ty) {
                unassigned.push(name.clone());
            }
        }
        if super::counted::sort_unstable_by(&self.meter, &mut unassigned, Ord::cmp).is_err() {
            self.release(held);
            self.leave_frame(body);
            self.leave_frame(previous);
            return;
        }
        let roster: super::construction::Roster = unassigned.into();
        // Each default sees the same set, less what the ones before it
        // assign, shared with the uses of `self` in them rather than copied.
        let mut building = super::construction::Unassigned::all(roster);
        for (stmt, assigned) in defaults.iter() {
            self.frame.building = Some(building.clone());
            self.stmt(stmt, Want::Discard);
            if let Some(name) = assigned {
                // What taking it out copies is counted first, and kept with
                // the checker's other growth.
                if self.meter.tables().keep(building.cost(name)).is_err() {
                    break;
                }
                self.grown += building.remove(name);
            }
        }
        self.frame.building = None;
        self.release(held);
        self.leave_frame(body);
        self.leave_frame(previous);
        for (name, state) in changes {
            if let Some(id) = self.local(&name) {
                self.frame.flow.set(id, state);
            }
        }
    }

    fn check_function(&mut self, id: FnId) {
        // Checking a function is a step, whatever its body holds.
        if self.meter.charge(1) {
            return;
        }
        let decl = &self.program.fns[id];
        let Some(def) = decl.def else {
            return;
        };
        let sig = decl.sig.clone();
        let owner = decl.owner;
        let instance = decl.instance;
        let main = decl.main;
        let accessor = def.accessor.is_some();
        // The frame keeps a copy of the function's name, counted before it
        // is made.
        if self.meter.tables().keep(sig.name.len()).is_err() {
            return;
        }
        let mut frame = Frame::new(&self.meter, owner, instance, sig.result, sig.name.clone());
        frame.main = main;
        frame.function = Some(id);
        frame.block = sig.block.clone();
        let previous = self.enter_frame(frame);
        if self.program.file && !main {
            // The file's locals are copied, and counted before they are.
            if self.transient(self.program.file_locals.heap()) {
                self.leave_frame(previous);
                return;
            }
            let mut locals = self.program.file_locals.clone().into_map();
            // Each function declares every one of the file's locals, a step
            // each, but those its parameters name, which are its own.
            if self.meter.charge(locals.len() as u64) {
                self.leave_frame(previous);
                return;
            }
            for param in def.params.iter() {
                locals.remove(param.name.as_str());
            }
            for (name, (ty, offset)) in locals {
                let Some(id) = self.declare(&name, ty, offset, true) else {
                    self.leave_frame(previous);
                    return;
                };
                self.assign_local(id, ty);
                // Declared in turn, so listed in the order of their ids.
                debug_assert!(self.frame.shared.last().is_none_or(|&last| last < id));
                if self.frame.shared.push(self.meter.tables(), id).is_err() {
                    self.leave_frame(previous);
                    return;
                }
            }
        }
        if instance && def.name == "initialize" {
            self.track_initialize(owner);
        }
        for (param, declared) in def.params.iter().zip(&sig.params) {
            if let Some(default) = &param.default {
                let mark = self.frame.flow.mark();
                self.symbols(None, |this| {
                    this.expr_against(
                        default,
                        declared.ty,
                        &Purpose::Local(this.copy(&param.name)),
                    )
                });
                let evaluated = self.frame.flow.rollback(mark);
                self.join(vec![
                    evaluated,
                    Branch {
                        live: true,
                        changes: Vec::new(),
                    },
                ]);
            }
            let Some(local) = self.declare(&param.name, declared.ty, def.offset as usize, true)
            else {
                self.leave_frame(previous);
                return;
            };
            self.assign_local(local, declared.ty);
            if let Some(ivar) = &param.ivar {
                let span = self
                    .spans
                    .word_after(def.offset as usize, &text!(self, "@{ivar}"));
                self.assign_ivar(ivar, declared.ty, span, accessor);
            }
        }
        if sig.block.as_ref().is_some_and(|block| block.optional) {
            self.frame.block_given = self.pseudo_local();
        }
        if accessor {
            // Properties read and write their declared instance variable.
            if let Some((name, false)) = &def.accessor {
                self.read_ivar(name, Span::at(def.offset as usize));
            }
            self.leave_frame(previous);
            return;
        }
        let want = match (main, sig.result) {
            (true, _) => Want::Infer(None),
            (false, Some(result)) => Want::Check(result),
            (false, None) => Want::Discard,
        };
        let body = &def.body;
        let result = self.stmts(body, want);
        // A check past its budget keeps neither what a session declares
        // nor a required file's locals, and checks nothing more here.
        let mut stopped = self.halted();
        if main && !self.program.file && self.annotate && !stopped {
            // Counted before they are copied.
            let (count, bytes) = self.assigned_size();
            stopped = self.transient(count * std::mem::size_of::<(String, Ty)>() + bytes);
            if !stopped {
                let mut locals = Vec::with_capacity(count);
                locals.extend(
                    self.assigned_locals()
                        .map(|(name, id)| (name.clone(), self.frame.locals[id as usize].declared)),
                );
                self.session = Some(super::Session { locals, result });
                let locals = self
                    .session
                    .as_ref()
                    .map_or(0, |session| session.locals.heap());
                stopped = self.grow(locals);
            }
        }
        if main && self.program.file && !stopped {
            // Counted, with room for them, before they are copied.
            let (count, bytes) = self.assigned_size();
            let declarations = self.meter.declarations();
            let kept = declarations.keep(bytes);
            stopped = kept.is_err()
                || self
                    .program
                    .file_locals
                    .reserve(declarations, count)
                    .is_err();
            if let (Ok(mut kept), false) = (kept, stopped) {
                for (name, &id) in &self.frame.names {
                    if self.frame.flow.get(id).assigned {
                        let local = &self.frame.locals[id as usize];
                        self.program.file_locals.insert_kept(
                            &mut kept,
                            name.clone(),
                            (local.declared, local.offset),
                        );
                    }
                }
                stopped = self.declared();
            }
        }
        if stopped {
            self.leave_frame(previous);
            return;
        }
        if self.frame.flow.live {
            if let (Some(result), false) = (sig.result, main) {
                if body.is_empty() && !self.types.assignable(Ty::NIL, result) {
                    let span = self.spans.token(def.offset as usize);
                    let expected = self.types.display(result);
                    self.report(
                        Diagnostic::error(
                            Code::TYPE_MISMATCH,
                            span,
                            text!(
                                self,
                                "`{}` returns {expected}, but its body is empty and returns nil",
                                def.name
                            ),
                        )
                        .with_types(expected, "nil"),
                    );
                }
            }
            self.finish_initialize(Span::at(def.offset as usize));
        }
        self.leave_frame(previous);
    }

    /// Declares the instance variables `initialize` must assign.
    fn track_initialize(&mut self, owner: Option<NsId>) {
        let Some(ns) = owner else {
            return;
        };
        // The variables without defaults, with a copy of each name, and the
        // roster of those each path must assign, which takes the copies,
        // are held while they are listed.
        let ivars = &self.program.namespaces[ns as usize].ivars;
        let (count, bytes) = ivars
            .iter()
            .filter(|(_, ivar)| !ivar.default)
            .fold((0, 0), |(count, bytes), (name, _)| {
                (count + 1, bytes + name.len())
            });
        let entries = std::mem::size_of::<(String, Ty)>() + 2 * std::mem::size_of::<String>();
        let Some(held) = self.hold(count * entries + bytes) else {
            return;
        };
        let mut required: Vec<(String, Ty)> = Vec::with_capacity(count);
        required.extend(
            self.program.namespaces[ns as usize]
                .ivars
                .iter()
                .filter(|(_, ivar)| !ivar.default)
                .map(|(name, ivar)| (name.clone(), ivar.ty)),
        );
        if super::counted::sort_unstable_by(&self.meter, &mut required, |a, b| a.0.cmp(&b.0))
            .is_err()
        {
            self.release(held);
            return;
        }
        let mut roster = Vec::with_capacity(count);
        let mut first = None;
        for (name, ty) in required {
            if self.types.assignable(Ty::NIL, ty) {
                continue;
            }
            // A check the budget stops tracks none of them.
            let Some(id) = self.pseudo_local() else {
                self.release(held);
                return;
            };
            first.get_or_insert(id);
            roster.push(name);
        }
        // The roster the frame keeps is counted as the frame's before the
        // copies held for it are let go.
        let roster: super::construction::Roster = roster.into();
        let kept = self
            .meter
            .tables()
            .keep(super::construction::roster_bytes(&roster))
            .is_ok();
        self.release(held);
        // The pseudo-locals follow each other, in the roster's order.
        if let (Some(first), true) = (first, kept) {
            self.frame.flow.track(first, roster.len());
            self.frame.initialize = Some((roster, first));
        }
    }

    /// Reports the instance variables a path through `initialize` left unassigned.
    pub(super) fn finish_initialize(&mut self, span: Span) {
        let Some((roster, first)) = self.frame.initialize.clone() else {
            return;
        };
        let Some(unassigned) = self.frame.flow.unassigned() else {
            return;
        };
        if unassigned.is_empty() {
            return;
        }
        // Their places, counted before they are listed.
        if self.transient(unassigned.len() * std::mem::size_of::<usize>()) {
            return;
        }
        let places = unassigned.indices();
        let (missing, count) = super::listed(&self.meter, places, |out, place| {
            out.push('@');
            out.push_str(&roster[place]);
        });
        // Report each variable once per function.
        self.frame.flow.untrack();
        self.frame.initialize = None;
        for place in 0..roster.len() {
            let id = first + place as LocalId;
            let state = self.frame.flow.get(id);
            self.frame.flow.set(
                id,
                VarState {
                    assigned: true,
                    ..state
                },
            );
        }
        self.report(Diagnostic::error(
            Code::UNINITIALIZED_IVAR,
            span,
            text!(self,
                "`initialize` does not assign {missing} on every path; assign {} or give {} a default in the class body",
                if count == 1 { "it" } else { "them" },
                if count == 1 { "it" } else { "them" },
            ),
        ));
    }

    /// The frame's locals assigned so far, by name.
    fn assigned_locals(&self) -> impl Iterator<Item = (&String, LocalId)> + '_ {
        self.frame
            .names
            .iter()
            .filter(|(_, id)| self.frame.flow.get(**id).assigned)
            .map(|(name, &id)| (name, id))
    }

    /// How many of the frame's locals are assigned so far, and the bytes
    /// of their names.
    fn assigned_size(&self) -> (usize, usize) {
        self.assigned_locals()
            .fold((0, 0), |(count, bytes), (name, _)| {
                (count + 1, bytes + name.len())
            })
    }

    /// Notes a call of script code, `callee` when it is one of this file's
    /// functions or methods, at `span`. In a required file the code may
    /// assign the file's top-level locals, so their narrowing ends; in the
    /// file's body it may read them before the body assigns them, which
    /// [`Self::check_file_calls`] reports once every function is checked.
    pub(super) fn script_called(&mut self, callee: Option<FnId>, span: Span) {
        // A check past its budget records no more calls, each of which
        // lists the locals assigned so far.
        if !self.program.file || self.halted() {
            return;
        }
        // The names are read in place: the frame, not the program, changes.
        for name in &self.program.file_written {
            if self.meter.charge(1) {
                return;
            }
            if let Some(&id) = self.frame.names.get(name.as_str()) {
                let state = self.frame.flow.get(id);
                let declared = self.frame.locals[id as usize].declared;
                self.frame.flow.set(
                    id,
                    VarState {
                        ty: declared,
                        ..state
                    },
                );
            }
        }
        let Some(callee) = callee else {
            return;
        };
        if self.frame.main {
            if !self.frame.flow.live {
                return;
            }
            // The locals assigned so far, counted before they are copied.
            let (count, bytes) = self.assigned_size();
            if self.transient(count * std::mem::size_of::<String>() + bytes) {
                return;
            }
            let mut assigned: Vec<String> = Vec::with_capacity(count);
            assigned.extend(self.assigned_locals().map(|(name, _)| name.clone()));
            // Sorted, so each name the callee reads is found by search.
            if super::counted::sort_unstable_by(&self.meter, &mut assigned, Ord::cmp).is_err()
                || self.meter.charge(assigned.len() as u64)
            {
                return;
            }
            let bytes = assigned.heap();
            let call = FileCall {
                callee,
                span,
                assigned,
            };
            // A call the budget refuses room for is not recorded, and the
            // check stops; one it records is counted, with the names it
            // keeps, as it is kept, and by the measures after.
            if self
                .program
                .file_calls
                .push(self.meter.tables(), call)
                .is_ok()
            {
                self.grown += bytes;
            } else {
                self.stopped = true;
            }
        } else if let Some(caller) = self.frame.function {
            // The caller's entry, and room for the call, are counted before
            // they are kept, and what its list grows by is kept with the
            // checker's growth; a check that stops records no more calls.
            let tables = self.meter.tables();
            let Ok((_, callees)) =
                self.program
                    .file_uses
                    .get_or_insert_with(tables, caller, Default::default)
            else {
                return;
            };
            let before = callees.capacity();
            if callees.push(tables, callee).is_err() {
                return;
            }
            let grown = (callees.capacity() - before) * std::mem::size_of::<FnId>();
            self.grown += grown;
        }
    }

    /// Notes a read of local `id` in a required file's function or method,
    /// when it is one of the file's top-level locals.
    pub(super) fn shared_read(&mut self, id: LocalId, name: &str) {
        // The file's locals are listed in the order of their ids.
        if self.frame.shared.binary_search(&id).is_ok() {
            if let Some(function) = self.frame.function {
                // The function's entry, and the name with its room in the
                // set, are counted before they are kept, and kept with the
                // checker's growth; a check that stops records no more
                // reads.
                let tables = self.meter.tables();
                let Ok((reads, _)) =
                    self.program
                        .file_uses
                        .get_or_insert_with(tables, function, Default::default)
                else {
                    return;
                };
                if reads.contains(name) {
                    return;
                }
                let before = super::meter::btree_storage::<String>(reads.len());
                let Ok(mut kept) = tables.keep(name.len()) else {
                    return;
                };
                if reads
                    .insert_kept(tables, &mut kept, name.to_owned())
                    .is_err()
                {
                    return;
                }
                let grown =
                    super::meter::btree_storage::<String>(reads.len()) - before + name.len();
                self.grown += grown;
            }
        }
    }

    /// Reports each call in a required file's body that runs code reading
    /// a top-level local the body has not assigned yet, which reads nil or
    /// is undefined.
    fn check_file_calls(&mut self) {
        let mut uses = std::mem::take(&mut self.program.file_uses);
        // Taken from the program, the table is held while it is read.
        let Some(taken) = self.hold(super::meter::map(&uses)) else {
            return;
        };
        // A copy of what each function reads, counted before it is made.
        if self.transient(
            super::meter::table::<(FnId, std::collections::BTreeSet<String>)>(uses.len())
                + uses.values().map(|(read, _)| read.heap()).sum::<usize>(),
        ) {
            self.release(taken);
            return;
        }
        let mut reads: HashMap<FnId, std::collections::BTreeSet<String>> = uses
            .iter()
            .map(|(&id, (read, _))| (id, (**read).clone()))
            .collect();
        if self.grow(reads.heap()) {
            self.release(taken);
            return;
        }
        // A function called many times reads the same variables at every
        // call, so each caller's callees are taken once each.
        for (_, callees) in uses.values_mut() {
            if super::counted::sort_unstable_by(&self.meter, callees, Ord::cmp).is_err() {
                self.release(taken);
                return;
            }
            callees.dedup();
        }
        // Checking each call charged for the first pass over the calls.
        let mut again = false;
        let mut changed = true;
        while changed {
            changed = false;
            for (&caller, (_, callees)) in &uses {
                if self.over_budget() {
                    self.release(taken);
                    return;
                }
                // The caller's variables are set aside while each callee's
                // are added to them, which are read in place rather than
                // copied, as only the names the caller lacks are.
                let mut entry = reads.remove(&caller).unwrap_or_default();
                for callee in callees {
                    if self.meter.charge(u64::from(again)) {
                        self.release(taken);
                        return;
                    }
                    let Some(read) = reads.get(callee) else {
                        continue;
                    };
                    if self.meter.charge(read.len() as u64) {
                        self.release(taken);
                        return;
                    }
                    for name in read {
                        if entry.contains(name) {
                            continue;
                        }
                        // The copy, and its room in the set, are counted
                        // before it is made.
                        let bytes = super::meter::btree_entry(&entry) + name.len();
                        if self.grow(bytes) {
                            self.release(taken);
                            return;
                        }
                        entry.insert(name.clone());
                        changed = true;
                    }
                }
                reads.insert(caller, entry);
            }
            again = true;
        }
        let calls = std::mem::take(&mut self.program.file_calls);
        let Some(calls_held) = self.hold(super::meter::vec(calls.as_vec())) else {
            self.release(taken);
            return;
        };
        let taken = taken + calls_held;
        for call in calls {
            let Some(read) = reads.get(&call.callee) else {
                continue;
            };
            if self.meter.charge(read.len() as u64) {
                break;
            }
            let Some(first) = read
                .iter()
                .find(|name| call.assigned.binary_search(name).is_err())
            else {
                continue;
            };
            let callee = self.program.fns[call.callee]
                .def
                .map_or_else(String::new, |def| self.copy(&def.name));
            self.report(Diagnostic::error(
                Code::UNASSIGNED_LOCAL,
                call.span,
                text!(self,
                    "`{callee}` reads `{first}`, which the file has not assigned on every path that reaches this call; assign it first"
                ),
            ));
        }
        self.release(taken);
    }

    /// Runs `check` with symbol literals made enum members where `stay` is
    /// `None`, as at a typed boundary the runtime checks, and reported with
    /// `stay`'s reason where the runtime keeps them symbols.
    pub(super) fn symbols<T>(
        &mut self,
        stay: Option<&'static str>,
        check: impl FnOnce(&mut Self) -> T,
    ) -> T {
        let outer = std::mem::replace(&mut self.symbols_stay, stay);
        let result = check(self);
        self.symbols_stay = outer;
        result
    }

    /// The declared type of the host global a write to `name` updates:
    /// one the host declares, which no local, parameter or block parameter
    /// of the name shadows. The runtime writes the global's binding, which
    /// every function then reads, so the write keeps the declared type. A
    /// required file reads the globals but never writes them: its writes
    /// bind the file's own variables, which shadow them.
    pub(super) fn host_global(&self, name: &str) -> Option<Ty> {
        if self.program.file || name.starts_with('@') || self.local(name).is_some() {
            return None;
        }
        self.program.declared.get(name).copied()
    }

    /// Checks a value of type `ty` written to host global `name`, which
    /// the host declares as `declared`.
    fn global_write(&mut self, name: &str, declared: Ty, ty: Ty, span: Span) {
        if !self.types.assignable(ty, declared) {
            self.mismatch(span, declared, ty, &Purpose::Global(self.copy(name)));
        }
    }

    /// Checks a typed declaration of host global `name`, which the host
    /// declares as `global`, with the `declared` type, which must be the
    /// global's own, since the write keeps it; `false` once reported.
    fn global_declaration(&mut self, name: &str, global: Ty, declared: Ty, span: Span) -> bool {
        if global == declared || declared == Ty::ERROR {
            return true;
        }
        let first = self.types.display(global);
        self.report(
            Diagnostic::error(
                Code::LOCAL_TYPE_CHANGED,
                span,
                text!(self,
                    "the host declares the global `{name}` as {first}, and assigning it writes the global, which keeps that type; assign it without a type"
                ),
            )
            .with_types(first, self.types.display(declared)),
        );
        false
    }

    // Locals -----------------------------------------------------------

    /// Declares a local in the innermost scope; `None` when the budget
    /// refuses it room, which stops the check and declares nothing.
    pub(super) fn declare(
        &mut self,
        name: &str,
        declared: Ty,
        offset: usize,
        annotated: bool,
    ) -> Option<LocalId> {
        // The local, its entry by name and its scope's record each copy its
        // name. They, and room in each table they go in, are counted before
        // any table changes, so a refusal leaves the frame as it was.
        let tables = self.meter.tables();
        let frame = &mut self.frame;
        let mut kept = tables.keep(3 * name.len()).ok()?;
        frame.flow.reserve().ok()?;
        frame.locals.reserve(tables, 1).ok()?;
        if !frame.names.contains_key(name) {
            frame.names.reserve(tables, 1).ok()?;
        }
        if let Some(scope) = frame.scopes.last_mut() {
            scope.reserve(tables, 1).ok()?;
        }
        let id = frame.flow.add(declared);
        debug_assert_eq!(id as usize, frame.locals.len());
        frame.locals.push_kept(
            &mut kept,
            Local {
                name: name.to_owned(),
                declared,
                offset,
                annotated,
                checked: false,
                dictionary: None,
            },
        );
        let previous = frame.names.insert_kept(&mut kept, name.to_owned(), id);
        frame.name_bytes += 3 * name.len();
        if let Some(scope) = frame.scopes.last_mut() {
            scope.push_kept(&mut kept, (name.to_owned(), previous));
        }
        Some(id)
    }

    /// A flow fact that is not a named local, such as whether a block was
    /// given; `None` when the budget refuses it room, which stops the
    /// check and adds nothing.
    fn pseudo_local(&mut self) -> Option<LocalId> {
        let tables = self.meter.tables();
        self.frame.flow.reserve().ok()?;
        self.frame.locals.reserve(tables, 1).ok()?;
        let id = self.frame.flow.add(Ty::BOOL);
        self.frame.locals.push_within(Local {
            name: String::new(),
            declared: Ty::BOOL,
            offset: 0,
            annotated: true,
            checked: false,
            dictionary: None,
        });
        Some(id)
    }

    pub(super) fn local(&self, name: &str) -> Option<LocalId> {
        self.frame.names.get(name).copied()
    }

    /// Records an assignment of a value of type `ty`, narrowing the local to it.
    pub(super) fn assign_local(&mut self, id: LocalId, ty: Ty) {
        let declared = self.frame.locals[id as usize].declared;
        let narrowed = self.narrowed(declared, ty);
        self.frame.flow.set(
            id,
            VarState {
                ty: narrowed,
                assigned: true,
            },
        );
    }

    /// The alternatives of `declared` that a value of type `ty` may be.
    pub(super) fn narrowed(&mut self, declared: Ty, ty: Ty) -> Ty {
        if ty == Ty::ERROR || declared == Ty::ERROR || declared == Ty::ANY {
            return declared;
        }
        if !matches!(self.types.kind(declared), Kind::Union(_)) {
            return declared;
        }
        let kept = self.types.meet(declared, ty);
        if kept.is_empty() {
            declared
        } else {
            self.types.union(&kept)
        }
    }

    /// Opens a block scope; whether it did. A scope the budget refuses
    /// room for is not opened, and must not be closed: the check has
    /// stopped.
    #[must_use = "a scope the budget refuses is not opened, and must not be closed"]
    pub(super) fn open_scope(&mut self) -> bool {
        self.frame
            .scopes
            .push(self.meter.tables(), CountedVec::new())
            .is_ok()
    }

    pub(super) fn close_scope(&mut self) {
        let Some(scope) = self.frame.scopes.pop() else {
            return;
        };
        // A name a scope shadowed had its entry, which it gets back in
        // place.
        for (name, previous) in scope.into_iter().rev() {
            match previous {
                Some(id) => {
                    if let Some(entry) = self.frame.names.get_mut(&name) {
                        *entry = id;
                    }
                }
                None => {
                    self.frame.names.remove(&name);
                }
            }
        }
    }

    pub(super) fn apply(&mut self, narrowings: &[(LocalId, Ty)]) {
        for &(id, ty) in narrowings {
            let state = self.frame.flow.get(id);
            if Some(id) == self.frame.block_given {
                // Where `block_given?` holds, the block counts as given.
                self.frame.flow.set(id, VarState { ty, assigned: true });
                continue;
            }
            if !state.assigned {
                continue;
            }
            self.frame.flow.set(id, VarState { ty, ..state });
        }
    }

    /// Keeps `branch`, one end of a construct's branches, in `explored`, a
    /// list counted, its storage and what each branch owns, while it lives,
    /// until [`Self::join_explored`] joins them.
    pub(super) fn explore(&mut self, explored: &mut ScratchVec<Branch>, branch: Branch) {
        // A check past its budget unwinds without keeping branches, which
        // its joins would not read; a refusal stops it.
        explored.add(branch);
    }

    /// Joins the branches [`Self::explore`] kept.
    pub(super) fn join_explored(&mut self, explored: ScratchVec<Branch>) {
        self.join(explored.into_vec());
    }

    pub(super) fn join(&mut self, branches: Vec<Branch>) {
        // A stopped check unwinds without the work its budget ran out of.
        if self.halted() {
            return;
        }
        // The branches, taken from wherever they were kept, and a table of
        // each one's changes are held while the join runs.
        let Some(held) = self.hold(branches.heap() + Flow::join_scratch(&branches)) else {
            return;
        };
        // The locals' declared types are read in place, not copied at every
        // join.
        let locals = &self.frame.locals;
        let lookup = |id: LocalId| {
            locals
                .get(id as usize)
                .map_or(Ty::BOOL, |local| local.declared)
        };
        self.frame.flow.join(&mut self.types, branches, &lookup);
        self.release(held);
    }

    /// Widens the locals a loop body assigns back to their declared types,
    /// since the body may run again after narrowing them.
    pub(super) fn widen_for_loop(&mut self, body: &'a [Stmt]) {
        let span = self.assigns.body(&self.meter, body);
        if self.meter.charge(body.len() as u64) {
            return;
        }
        self.widen(span);
    }

    /// Widens each local in scope that an assignment in `span` writes to its
    /// declared type, which forgets its narrowing but not whether it is
    /// assigned.
    pub(super) fn widen(&mut self, span: super::assigns::Span) {
        if self.halted() {
            return;
        }
        let names = self.assigns.distinct(span, |count, _| {
            !self.transient(count * std::mem::size_of::<&str>())
        });
        if self.meter.charge(names.len() as u64) {
            return;
        }
        for name in names {
            if let Some(id) = self.local(name) {
                let state = self.frame.flow.get(id);
                let declared = self.frame.locals[id as usize].declared;
                self.frame.flow.set(
                    id,
                    VarState {
                        ty: declared,
                        ..state
                    },
                );
            }
        }
    }

    // Statements -------------------------------------------------------

    /// Checks statements in order; the last one's value is `want`ed.
    pub(super) fn stmts(&mut self, body: &'a [Stmt], want: Want) -> Ty {
        let Some((last, rest)) = body.split_last() else {
            return Ty::NIL;
        };
        for stmt in rest {
            self.stmt(stmt, Want::Discard);
        }
        let ty = self.statement(last, want, true);
        self.too_large(last.offset as usize);
        ty
    }

    pub(super) fn stmt(&mut self, stmt: &'a Stmt, want: Want) -> Ty {
        let ty = self.statement(stmt, want, false);
        // A type the statement inferred too large to build, such as the
        // union of a literal's many shapes, is reported at it.
        self.too_large(stmt.offset as usize);
        ty
    }

    /// Checks a statement, `last` when it ends a body, where a loop gives
    /// another value than as an expression.
    fn statement(&mut self, stmt: &'a Stmt, want: Want, last: bool) -> Ty {
        if self.meter.charge(1) || self.over_budget() {
            return Ty::ERROR;
        }
        if super::too_tall(stmt.height()) {
            // The statement's first token: its whole span is as deep as it.
            let span = self.spans.token(stmt.offset as usize);
            self.too_deep(span);
            return Ty::ANY;
        }
        if !self.frame.flow.live {
            // Unreachable code is still checked, from a live state.
            self.frame.flow.live = true;
        }
        match &stmt.node {
            Statement::Expr(expr) => match want {
                Want::Discard => {
                    self.expr_want(expr, Want::Discard);
                    Ty::NIL
                }
                Want::Infer(hint) => self.expr(expr, hint),
                Want::Check(expected) => {
                    let purpose = self.purposes.last().cloned().unwrap_or(Purpose::Result);
                    self.expr_against(expr, expected, &purpose)
                }
            },
            Statement::Assign(target, op, value) => {
                self.mark_target(target);
                let ty = self.assignment(stmt, target, op, value);
                self.statement_value(stmt, ty, want)
            }
            Statement::If(branches, alternate, _) => {
                self.if_statement(stmt.offset as usize, branches, alternate, want)
            }
            Statement::While(condition, body, _) => {
                let last = last && !matches!(want, Want::Discard);
                let ty = self.while_loop(condition, body, last);
                self.loop_value(stmt, ty, want, last)
            }
            Statement::For(target, iterable, body) => {
                let last = last && !matches!(want, Want::Discard);
                let ty = self.for_loop(target, iterable, body, last);
                self.loop_value(stmt, ty, want, last)
            }
            Statement::Return(value) => {
                self.return_statement(stmt, value.as_ref());
                Ty::NEVER
            }
            Statement::Break(value) => {
                self.break_statement(stmt, value.as_ref());
                Ty::NEVER
            }
            Statement::Next(value) => {
                self.next_statement(stmt, value.as_ref());
                Ty::NEVER
            }
            Statement::Raise(value, message) => {
                self.raise(value.as_deref(), message.as_deref());
                self.frame.flow.live = false;
                Ty::NEVER
            }
            Statement::Retry => {
                self.frame.flow.live = false;
                Ty::NEVER
            }
            Statement::Module(name) => {
                if let Some(&ns) = self.program.roots.get(name.as_str()) {
                    self.check_namespace_body(ns);
                }
                self.statement_value(stmt, Ty::NIL, want)
            }
            // The runtime refuses a declaration nested where it cannot bind
            // one, whenever the statement runs.
            Statement::UnboundClass(_) => {
                self.nested_declaration(
                    stmt,
                    "class declarations are only supported at the top level",
                );
                self.statement_value(stmt, Ty::NIL, want)
            }
            Statement::Unsupported => {
                self.nested_declaration(
                    stmt,
                    "function declarations are only supported at the top level or in class and module bodies",
                );
                self.statement_value(stmt, Ty::NIL, want)
            }
        }
    }

    /// Reports a declaration nested in a body, which the runtime refuses
    /// when it runs.
    fn nested_declaration(&mut self, stmt: &Stmt, message: &str) {
        let span = self.spans.token(stmt.offset as usize);
        self.report(Diagnostic::error(Code::SYNTAX, span, message));
    }

    /// Checks a statement's value against what is wanted of it.
    fn statement_value(&mut self, stmt: &Stmt, ty: Ty, want: Want) -> Ty {
        if let Want::Check(expected) = want {
            if self.frame.flow.live && !self.types.assignable(ty, expected) {
                let span = self.spans.stmt(stmt);
                let purpose = self.purposes.last().cloned().unwrap_or(Purpose::Result);
                self.mismatch(span, expected, ty, &purpose);
            }
        }
        ty
    }

    fn if_statement(
        &mut self,
        if_offset: usize,
        branches: &'a [(Expr, crate::compilation::Buffer<Stmt>)],
        alternate: &'a [Stmt],
        want: Want,
    ) -> Ty {
        // The branches' values, in a list counted while it lives.
        let mut results = super::counted::ScratchVec::new(&self.meter);
        let mut explored = ScratchVec::new(&self.meter);
        let entry = self.frame.flow.mark();
        for (condition, body) in branches {
            let narrow = self.condition(condition);
            let mark = self.frame.flow.mark();
            self.apply(&narrow.then);
            let ty = self.stmts(body, want);
            // A check past its budget unwinds without the work of the
            // branches it nests in.
            if self.halted() {
                self.frame.flow.rollback(entry);
                return Ty::ERROR;
            }
            if self.frame.flow.live {
                results.add(ty);
            }
            let branch = self.frame.flow.rollback(mark);
            self.explore(&mut explored, branch);
            self.apply(&narrow.otherwise);
        }
        let ty = if alternate.is_empty() {
            if let (Want::Check(expected), true) = (want, self.frame.flow.live) {
                if !self.types.assignable(Ty::NIL, expected) {
                    let span = self.spans.token(if_offset);
                    let expected_text = self.types.display(expected);
                    let purpose = self.purposes.last().cloned().unwrap_or(Purpose::Result);
                    let what = self.purpose_text(&purpose, &expected_text);
                    self.report(
                        Diagnostic::error(
                            Code::TYPE_MISMATCH,
                            span,
                            text!(self, "{what}, but this `if` has no `else`, so it gives nil when no branch runs"),
                        )
                        .with_types(expected_text, "nil"),
                    );
                }
            }
            Ty::NIL
        } else {
            self.stmts(alternate, want)
        };
        if self.frame.flow.live {
            results.add(ty);
        }
        let branch = self.frame.flow.rollback(entry);
        self.explore(&mut explored, branch);
        self.join_explored(explored);
        self.types.union(&results)
    }

    /// Checks a loop's value against what is wanted of it, explaining the
    /// value of one that ends a body.
    fn loop_value(&mut self, stmt: &Stmt, ty: Ty, want: Want, last: bool) -> Ty {
        let Want::Check(expected) = want else {
            return ty;
        };
        if !last || !self.frame.flow.live || self.types.assignable(ty, expected) {
            return self.statement_value(stmt, ty, want);
        }
        let span = self.spans.token(stmt.offset as usize);
        let expected_text = self.types.display(expected);
        let found = self.types.display(ty);
        let purpose = self.purposes.last().cloned().unwrap_or(Purpose::Result);
        let what = self.purpose_text(&purpose, &expected_text);
        let without = self.types.without_nil(ty);
        let (code, message) = if self.types.has_nil(ty)
            && without != Ty::NEVER
            && self.types.assignable(without, expected)
        {
            (
                Code::OPTIONAL_USE,
                text!(
                    self,
                    "{what}, but a loop that ends a body gives nil when no iteration reaches the end of its body; end the body with the value it should give"
                ),
            )
        } else {
            (
                Code::TYPE_MISMATCH,
                text!(
                    self,
                    "{what}, but a loop that ends a body gives the value its body had last, or nil when it never ran, so this one gives {found}; end the body with the value it should give"
                ),
            )
        };
        self.report(Diagnostic::error(code, span, message).with_types(expected_text, found));
        ty
    }

    /// Checks a `while` loop and returns its value: `nil`, or what `break`
    /// gives. A loop that ends a body, `last`, gives the value its body had
    /// last instead of `nil`, as the runtime runs it.
    fn while_loop(&mut self, condition: &'a Expr, body: &'a [Stmt], last: bool) -> Ty {
        self.widen_for_loop(body);
        let before = self.frame.flow.mark();
        let infinite =
            matches!(&condition.node, Node::Literal(v) if v.type_name() == "bool" && v.truthy());
        let narrow = self.condition(condition);
        self.apply(&narrow.then);
        let context = Context::Loop {
            mark: before,
            exits: Exits::default(),
        };
        // A loop the budget refuses room for is not checked; its narrowing
        // is undone as its end would.
        if self
            .frame
            .contexts
            .push(self.meter.tables(), context)
            .is_err()
        {
            self.frame.flow.rollback(before);
            return Ty::ERROR;
        }
        let value = self.stmts(body, Self::body_want(last));
        let context = self.frame.contexts.pop().unwrap();
        let mut values = self.finish_loop(before, context, !infinite);
        // A `break` without a value leaves the loop's value as it was.
        if !infinite || (last && values.contains(&Ty::NIL)) {
            values.push(Ty::NIL);
            if last {
                values.push(value);
            }
        }
        self.types.union(&values)
    }

    /// What a loop's body gives: its value when the loop ends a body and
    /// gives the value its body had last.
    fn body_want(last: bool) -> Want {
        if last {
            Want::Infer(None)
        } else {
            Want::Discard
        }
    }

    /// Joins the states a loop or block can be left with and returns the
    /// types of the values `break` gave. A loop that ends by itself is left
    /// from its end, each `next` and, when it may not run, from before it;
    /// an infinite one only by `break`.
    pub(super) fn finish_loop(
        &mut self,
        before: Mark,
        mut context: Context,
        ends: bool,
    ) -> Vec<Ty> {
        let exits = std::mem::take(context.exits());
        let end = self.frame.flow.rollback(before);
        let mut branches = exits.breaks;
        if ends {
            let tables = self.meter.tables();
            // The state at the loop's end, which its exits did not keep, is
            // counted with room for it and them; a check the budget stops
            // joins none of them.
            let kept = tables.keep(super::counted::Owned::owned(&end));
            let Ok(mut kept) = kept else {
                return exits.values.into_vec();
            };
            if branches.reserve(tables, exits.nexts.len() + 2).is_err() {
                return exits.values.into_vec();
            }
            for branch in exits.nexts {
                branches.push_moved(branch);
            }
            branches.push_kept(&mut kept, end);
            branches.push_within(Branch {
                live: true,
                changes: Vec::new(),
            });
        }
        self.join(branches.into_vec());
        exits.values.into_vec()
    }

    /// Checks a `for` loop and returns its value: the iterable, or what
    /// `break` gives. A loop that ends a body, `last`, gives the value its
    /// body had last instead, or `nil` when no iteration reached the end of
    /// the body.
    fn for_loop(
        &mut self,
        target: &'a Target,
        iterable: &'a Expr,
        body: &'a [Stmt],
        last: bool,
    ) -> Ty {
        let ty = self.expr(iterable, None);
        let element = self.iterated(ty, iterable);
        self.widen_for_loop(body);
        let nonempty = matches!(&iterable.node, Node::Array(items) if !items.is_empty())
            || matches!(self.types.kind(ty), Kind::Tuple(items) if !items.is_empty());
        self.declare_for_target(target, element, nonempty);
        let before = self.frame.flow.mark();
        self.bind_target(target, element, true);
        let context = Context::Loop {
            mark: before,
            exits: Exits::default(),
        };
        // A loop the budget refuses room for is not checked; its bindings
        // are undone as its end would.
        if self
            .frame
            .contexts
            .push(self.meter.tables(), context)
            .is_err()
        {
            self.frame.flow.rollback(before);
            return Ty::ERROR;
        }
        let value = self.stmts(body, Self::body_want(last));
        let mut context = self.frame.contexts.pop().unwrap();
        let skips = !context.exits().nexts.is_empty();
        // A `break` gives the loop its value instead of the iterable.
        let mut values = self.finish_loop(before, context, true);
        if last {
            // A literal with elements, or a record with fields, runs the
            // body at least once.
            let runs = nonempty
                || matches!(self.types.kind(ty), Kind::Shape(fields, _) if fields.iter().any(|field| !field.optional));
            if skips || !runs {
                values.push(Ty::NIL);
            }
            values.push(value);
        } else if values.is_empty() {
            return ty;
        } else {
            values.push(ty);
        }
        self.types.union(&values)
    }

    fn declare_for_target(&mut self, target: &'a Target, element: Ty, nonempty: bool) {
        match target {
            Target::Value(expr) => {
                if let Node::Var(name) = &expr.node {
                    if self.local(name).is_none()
                        && !name.starts_with('@')
                        && self.host_global(name).is_none()
                    {
                        self.check_binding_target(target);
                        let declared = if nonempty {
                            element
                        } else {
                            self.types.optional(element)
                        };
                        if let Some(id) = self.declare(name, declared, expr.offset as usize, false)
                        {
                            self.assign_local(id, if nonempty { element } else { Ty::NIL });
                        }
                    }
                }
            }
            Target::Tuple(parts) => {
                let splat = parts.iter().position(|(_, rest)| *rest);
                for (index, (part, rest)) in parts.iter().enumerate() {
                    if let Some(part) = part {
                        let ty = if *rest {
                            self.rest_of(element, index, parts.len())
                        } else {
                            self.part_of(element, index, parts.len(), splat)
                        };
                        self.declare_for_target(part, ty, nonempty);
                    }
                }
            }
            Target::Typed(inner, _) => self.declare_for_target(inner, element, nonempty),
        }
    }

    /// The element type `for` binds when iterating a value of type `ty`.
    fn iterated(&mut self, ty: Ty, iterable: &Expr) -> Ty {
        if let Some(element) = self.types.element(ty) {
            return element;
        }
        if let Some(value) = self.types.hash_value(ty) {
            return self.types.tuple(vec![Ty::STRING, value]);
        }
        let span = self.spans.expr(iterable);
        let found = self.types.display(ty);
        self.report(
            Diagnostic::error(
                Code::TYPE_MISMATCH,
                span,
                text!(self, "`for` iterates an array, hash or range, not {found}"),
            )
            .with_types("array | hash | range", found),
        );
        Ty::ERROR
    }

    fn return_statement(&mut self, stmt: &'a Stmt, value: Option<&'a Expr>) {
        if self.frame.main {
            if let Some(value) = value {
                self.expr(value, None);
            }
            self.frame.flow.live = false;
            return;
        }
        match (self.frame.result, value) {
            (Some(result), Some(value)) => {
                self.symbols(None, |this| {
                    this.expr_against(value, result, &Purpose::Result)
                });
            }
            (Some(result), None) => {
                if !self.types.assignable(Ty::NIL, result) {
                    let span = self.spans.stmt(stmt);
                    self.mismatch(span, result, Ty::NIL, &Purpose::Result);
                }
            }
            (None, Some(value)) => {
                let ty = self.expr(value, None);
                let literal_nil = matches!(&value.node, Node::Literal(v) if v.type_name() == "nil");
                if !literal_nil && ty != Ty::ERROR {
                    let span = self.spans.stmt(stmt);
                    let found = self.types.display(ty);
                    self.report(Diagnostic::error(
                        Code::RETURN_WITHOUT_TYPE,
                        span,
                        text!(self,
                            "`{}` declares no result type, so it returns nil; declare `-> {found}` to return this value",
                            self.frame.name
                        ),
                    ));
                }
            }
            (None, None) => (),
        }
        if self.frame.flow.live {
            let span = self.spans.stmt(stmt);
            self.finish_initialize(span);
        }
        self.frame.flow.live = false;
    }

    /// `raise message`, `raise error`, `raise Class` or `raise Class, message`.
    fn raise(&mut self, value: Option<&'a Expr>, message: Option<&'a Expr>) {
        let class = |checker: &Self, expr: &Expr| match &expr.node {
            Node::Var(name) => {
                checker.local(name).is_none() && crate::ErrorClass::from_name(name).is_some()
            }
            _ => false,
        };
        if let Some(message) = message {
            self.expr_against(message, Ty::STRING, &Purpose::Operand);
            if let Some(value) = value {
                if !class(self, value) {
                    let ty = self.expr(value, None);
                    if ty != Ty::ERROR {
                        let span = self.spans.expr(value);
                        self.report(Diagnostic::error(
                            Code::TYPE_MISMATCH,
                            span,
                            "`raise` with a message takes an error class first, such as `raise ArgumentError, \"...\"`",
                        ));
                    }
                }
            }
            return;
        }
        let Some(value) = value else {
            return;
        };
        if class(self, value) {
            return;
        }
        let ty = self.expr(value, None);
        if ty == Ty::ERROR || ty == Ty::STRING || ty == Ty::ERROR_VALUE || ty == Ty::NEVER {
            return;
        }
        let span = self.spans.expr(value);
        let found = self.types.display(ty);
        self.report(
            Diagnostic::error(
                Code::TYPE_MISMATCH,
                span,
                text!(
                    self,
                    "`raise` takes a message, an error class or a rescued error, found {found}"
                ),
            )
            .with_types("string | error", found),
        );
    }

    fn break_statement(&mut self, stmt: &'a Stmt, value: Option<&'a Expr>) {
        self.check_loop_control(stmt, "break");
        let break_to = match self.frame.contexts.last() {
            Some(Context::Block { break_to, .. }) => break_to.clone(),
            _ => None,
        };
        let ty = match (value, break_to) {
            (Some(value), Some(to)) => {
                let purpose = Purpose::Break(to.function, to.inside);
                self.symbols(None, |this| this.expr_against(value, to.ty, &purpose))
            }
            (Some(value), None) => self.expr(value, None),
            (None, Some(to)) => {
                if self.frame.flow.live && !self.types.assignable(Ty::NIL, to.ty) {
                    let span = self.spans.stmt(stmt);
                    self.mismatch(
                        span,
                        to.ty,
                        Ty::NIL,
                        &Purpose::Break(to.function, to.inside),
                    );
                }
                Ty::NIL
            }
            (None, None) => Ty::NIL,
        };
        if self.frame.flow.live {
            let tables = self.meter.tables();
            if let Some(context) = self.frame.contexts.last_mut() {
                // A check the budget stops keeps no more of them.
                if context.exits().values.push(tables, ty).is_err() {
                    self.frame.flow.live = false;
                    return;
                }
            }
        }
        self.exit_context(true);
        self.frame.flow.live = false;
    }

    /// Records that a `break` in the caller's block leaves from this
    /// `yield`: it ends the innermost loop or block call around it with a
    /// value of the function's result type, or of any type when it declares
    /// none ([`super::sigs::Breaks::Inside`]), which the function's own code
    /// must accept, as the block of a script function must.
    pub(super) fn yield_breaks(&mut self, span: Span) {
        if !self.frame.flow.live {
            return;
        }
        let Some(context) = self.frame.contexts.last() else {
            return;
        };
        let mark = context.mark();
        let to = match context {
            Context::Block { break_to, .. } => break_to.clone(),
            Context::Loop { .. } => None,
        };
        let value = self.frame.result.unwrap_or(Ty::ANY);
        if let Some(to) = to {
            if !self.types.assignable(value, to.ty) {
                let found = self.types.display(value);
                let expected = self.types.display(to.ty);
                let what = if to.inside {
                    text!(
                        self,
                        "ends a loop or block inside `{}`, which takes {expected}",
                        to.function
                    )
                } else {
                    text!(
                        self,
                        "returns from `{}`, which returns {expected}",
                        to.function
                    )
                };
                self.report(
                    Diagnostic::error(
                        Code::TYPE_MISMATCH,
                        span,
                        text!(self,
                            "a `break` out of the caller's block, a value of {found}, leaves this `yield` and {what}; move the `yield` out of the block, or give the functions results that fit"
                        ),
                    )
                    .with_types(expected, found),
                );
            }
        }
        let branch = self.frame.flow.peek(mark);
        let tables = self.meter.tables();
        let exits = self.frame.contexts.last_mut().unwrap().exits();
        // A check the budget stops records neither.
        if exits.values.reserve(tables, 1).is_ok() && exits.breaks.push(tables, branch).is_ok() {
            exits.values.push_within(value);
        }
    }

    /// Records the state at a `break` or `next` for the enclosing loop or block.
    fn exit_context(&mut self, leaves: bool) {
        if !self.frame.flow.live {
            return;
        }
        let Some(context) = self.frame.contexts.last() else {
            return;
        };
        let branch = self.frame.flow.peek(context.mark());
        let tables = self.meter.tables();
        let exits = self.frame.contexts.last_mut().unwrap().exits();
        let kept = if leaves {
            exits.breaks.push(tables, branch)
        } else {
            exits.nexts.push(tables, branch)
        };
        // A check the budget stops keeps no more of them.
        if kept.is_err() {
            self.frame.flow.live = false;
        }
    }

    fn next_statement(&mut self, stmt: &'a Stmt, value: Option<&'a Expr>) {
        self.check_loop_control(stmt, "next");
        let block = match self.frame.contexts.last() {
            Some(Context::Block {
                result, hint, used, ..
            }) => Some((*result, *hint, *used)),
            _ => None,
        };
        match block {
            Some((result, hint, used)) => {
                let ty = match (value, result) {
                    (Some(value), Some(result)) => {
                        self.expr_against(value, result, &Purpose::BlockResult)
                    }
                    (Some(value), None) => self.expr(value, hint),
                    (None, Some(result)) => {
                        if used && !self.types.assignable(Ty::NIL, result) {
                            let span = self.spans.stmt(stmt);
                            self.mismatch(span, result, Ty::NIL, &Purpose::BlockResult);
                        }
                        Ty::NIL
                    }
                    (None, None) => Ty::NIL,
                };
                let tables = self.meter.tables();
                if let Some(Context::Block { results, .. }) = self.frame.contexts.last_mut() {
                    // A check the budget stops keeps no more of them.
                    if results.push(tables, ty).is_err() {
                        self.frame.flow.live = false;
                        return;
                    }
                }
            }
            None => {
                if let Some(value) = value {
                    self.expr(value, None);
                }
            }
        }
        self.exit_context(false);
        self.frame.flow.live = false;
    }

    fn check_loop_control(&mut self, stmt: &Stmt, name: &str) {
        if self.frame.contexts.is_empty() {
            self.report(Diagnostic::error(
                Code::SYNTAX,
                self.spans.token(stmt.offset as usize),
                text!(self, "`{name}` is only valid inside a loop or block"),
            ));
        }
    }

    // Assignment -------------------------------------------------------

    /// Marks the reads an assignment to `target` writes through.
    fn mark_target(&mut self, target: &Target) {
        match target {
            Target::Value(Expr {
                node: Node::Index(receiver, _) | Node::Member(receiver, _),
                ..
            }) => self.mark_write_chain(receiver),
            Target::Typed(inner, _) => self.mark_target(inner),
            Target::Tuple(parts) => {
                for part in parts.iter().filter_map(|(part, _)| part.as_ref()) {
                    self.mark_target(part);
                }
            }
            Target::Value(_) => (),
        }
    }

    /// Marks `receiver` and the reads it goes through as reads a write
    /// goes through.
    pub(super) fn mark_write_chain(&mut self, receiver: &Expr) {
        let mut current = receiver;
        loop {
            // A check the budget stops marks no more.
            let tables = self.meter.tables();
            if self
                .write_chain
                .insert(tables, std::ptr::from_ref(current) as usize)
                .is_err()
            {
                return;
            }
            current = match &current.node {
                Node::Index(inner, _) | Node::Member(inner, _) | Node::Method(inner, ..) => inner,
                _ => return,
            };
        }
    }

    /// Whether a write goes through the read `expr`.
    pub(super) fn in_write_chain(&self, expr: &Expr) -> bool {
        self.write_chain
            .contains(&(std::ptr::from_ref(expr) as usize))
    }

    fn assignment(&mut self, stmt: &'a Stmt, target: &'a Target, op: &str, value: &'a Expr) -> Ty {
        if op != "=" {
            self.check_binding_target(target);
        }
        match op {
            "=" => self.assign(target, value),
            "||=" | "&&=" => {
                let outer = self.set_memo(Some(super::Memo::default()));
                let current = self.target_read(target);
                if current != Ty::ERROR && current != Ty::BOOL {
                    let span = self.target_span(target);
                    let found = self.types.display(current);
                    self.report(
                        Diagnostic::error(
                            Code::CONDITION_NOT_BOOL,
                            span,
                            text!(self,
                                "`{op}` tests its target, which must be a bool, found {found}; assign under an explicit nil test instead"
                            ),
                        )
                        .with_types("bool", found),
                    );
                }
                let ty = self.expr(value, Some(current));
                self.memo.get_mut().unwrap().replay = true;
                self.target_write(target, ty, value);
                self.restore_memo(outer);
                ty
            }
            _ => {
                let operator = &op[..op.len() - 1];
                // The write reuses the types the read found for the target's
                // receiver and selectors instead of checking them again.
                let outer = self.set_memo(Some(super::Memo::default()));
                let current = self.target_read(target);
                let right = self.expr(value, None);
                let span = self.spans.stmt(stmt);
                self.memo.get_mut().unwrap().replay = true;
                let result = match self.optional_element(target, op, value, current) {
                    Some(present) => {
                        self.binary_types(operator, present, right, span, Some((None, value)))
                    }
                    None => self.binary_types(
                        operator,
                        current,
                        right,
                        span,
                        Some((target_expr(target), value)),
                    ),
                };
                self.target_write(target, result, value);
                self.restore_memo(outer);
                result
            }
        }
    }

    /// Reports a compound assignment to an array element or hash entry
    /// that may be missing, `x[i] += v`, offering `x[i] = x.fetch(i) + v`,
    /// and returns the element's type without nil. The receiver's types
    /// replay from the target's read.
    fn optional_element(
        &mut self,
        target: &'a Target,
        op: &str,
        value: &'a Expr,
        current: Ty,
    ) -> Option<Ty> {
        let Target::Value(expr) = target else {
            return None;
        };
        let Node::Index(receiver, selectors) = &expr.node else {
            return None;
        };
        if selectors.len() != 1 || !self.types.has_nil(current) {
            return None;
        }
        let present = self.types.without_nil(current);
        if present == Ty::NEVER {
            return None;
        }
        let span = self.spans.expr(expr);
        let found = self.types.display(current);
        let mut diagnostic = Diagnostic::error(
            Code::OPTIONAL_USE,
            span,
            text!(
                self,
                "this element may be nil ({found}), as it is when missing; read it with `fetch`, which raises when it is missing, or test it with `!= nil` first"
            ),
        );
        let receiver_ty = self.expr(receiver, None);
        let stored = match *self.types.kind(receiver_ty) {
            Kind::Array(element) => Some(element),
            Kind::Hash(value) => Some(value),
            _ => None,
        };
        if stored.is_some_and(|stored| stored == present)
            && self.stable(receiver)
            && self.stable(&selectors[0])
        {
            if let Some(fix) = self.fetch_assignment(expr, receiver, &selectors[0], op, value) {
                diagnostic = diagnostic.with_fix(fix);
            }
        }
        self.report(diagnostic);
        Some(present)
    }

    /// Whether an expression reads the same value each time, so a fix may
    /// repeat it: a local, an instance variable, a literal, or an element
    /// of one at a key that is one, as in `grid[0]`.
    fn stable(&self, expr: &Expr) -> bool {
        match &expr.node {
            Node::Var(name) => name.starts_with('@') || self.local(name).is_some(),
            Node::Integer(_) | Node::Literal(_) => true,
            Node::Index(receiver, selectors) => {
                selectors.len() == 1 && self.stable(receiver) && self.stable(&selectors[0])
            }
            _ => false,
        }
    }

    /// `x[i] = x.fetch(i) + v` in place of `x[i] += v`.
    fn fetch_assignment(
        &self,
        target: &Expr,
        receiver: &Expr,
        selector: &Expr,
        op: &str,
        value: &Expr,
    ) -> Option<Fix> {
        let target_end = self.spans.expr(target).end;
        let value_span = self.spans.expr(value);
        let between = self.source.get(target_end..value_span.start)?;
        let at = target_end + between.find(op)?;
        let receiver_span = self.spans.expr(receiver);
        let selector_span = self.spans.expr(selector);
        let receiver_text = self.source.get(receiver_span.start..receiver_span.end)?;
        let selector_text = self.source.get(selector_span.start..selector_span.end)?;
        let operator = &op[..op.len() - 1];
        let grouped = matches!(
            value.node,
            Node::Integer(_)
                | Node::BigInteger(..)
                | Node::Literal(_)
                | Node::Template(..)
                | Node::Var(_)
                | Node::Array(_)
                | Node::Call(..)
                | Node::Method(..)
                | Node::Member(..)
                | Node::Index(..)
        );
        let (open, close) = if grouped { ("", "") } else { ("(", ")") };
        let mut edits = vec![
            Edit {
                span: Span::new(at, at + op.len()),
                replacement: "=".to_owned(),
            },
            Edit {
                span: Span::at(value_span.start),
                replacement: text!(
                    self,
                    "{receiver_text}.fetch({selector_text}) {operator} {open}"
                ),
            },
        ];
        if !close.is_empty() {
            edits.push(Edit {
                span: Span::at(value_span.end),
                replacement: close.to_owned(),
            });
        }
        Some(Fix::edits(
            text!(
                self,
                "read it with `{receiver_text}.fetch({selector_text})`"
            ),
            edits,
        ))
    }

    fn target_span(&self, target: &Target) -> Span {
        match target {
            Target::Value(expr) => self.spans.expr(expr),
            Target::Typed(inner, _) => self.target_span(inner),
            Target::Tuple(_) => Span::at(target.offset().unwrap_or(0) as usize),
        }
    }

    /// The current value of an assignment target, for compound assignment.
    fn target_read(&mut self, target: &'a Target) -> Ty {
        match target {
            Target::Value(expr) => self.expr(expr, None),
            Target::Typed(inner, _) => self.target_read(inner),
            Target::Tuple(_) => Ty::ERROR,
        }
    }

    /// Stores a value of type `ty` computed from `value` into a target.
    fn target_write(&mut self, target: &'a Target, ty: Ty, value: &'a Expr) {
        match target {
            Target::Value(expr) => match &expr.node {
                Node::Var(name) if name.starts_with("@@") => {
                    self.write_class_variable(name, ty, expr, value);
                }
                Node::Var(name) if name.starts_with('@') => {
                    let span = self.spans.expr(expr);
                    self.write_ivar(&name[1..], ty, span, value);
                }
                Node::Var(name) => {
                    if let Some(id) = self.local(name) {
                        let declared = self.frame.locals[id as usize].declared;
                        if !self.types.assignable(ty, declared) {
                            let span = self.spans.expr(value);
                            self.local_changed(id, span, ty);
                        }
                        self.assign_local(id, ty);
                    } else if let Some(global) = self.host_global(name) {
                        let span = self.spans.expr(value);
                        self.global_write(name, global, ty, span);
                    }
                }
                Node::Index(receiver, selectors) => {
                    self.index_write(expr, receiver, selectors, ty, value, false);
                }
                Node::Member(receiver, name) => {
                    self.setter(expr, receiver, name, ty, value, false);
                }
                _ => (),
            },
            Target::Typed(inner, _) => self.target_write(inner, ty, value),
            Target::Tuple(_) => (),
        }
    }

    fn assign(&mut self, target: &'a Target, value: &'a Expr) -> Ty {
        self.check_binding_target(target);
        match target {
            Target::Typed(inner, annotation) => {
                let Target::Value(Expr {
                    node: Node::Var(name),
                    offset,
                    ..
                }) = &**inner
                else {
                    let ty = self.expr(value, None);
                    self.bind_target(target, ty, true);
                    return ty;
                };
                let declared = self.annotation(annotation, self.frame.owner, *offset as usize);
                if let (Some(global), false) = (
                    self.host_global(name),
                    self.frame.namespace_body && is_constant(name),
                ) {
                    // A declaration writes the global too, whose type stays.
                    let span = self.spans.token(*offset as usize);
                    let purpose = if self.global_declaration(name, global, declared, span) {
                        Purpose::Global(self.copy(name))
                    } else {
                        Purpose::Local(self.copy(name))
                    };
                    return self.expr_against(value, declared, &purpose);
                }
                let ty = self.symbols(None, |this| {
                    this.expr_against(value, declared, &Purpose::Local(this.copy(name)))
                });
                if self.frame.namespace_body && is_constant(name) {
                    if self.keep_constant((self.frame.owner, self.copy(name)), declared) {
                        return Ty::ERROR;
                    }
                    return ty;
                }
                let id = match self.local(name) {
                    Some(id) => {
                        let existing = self.frame.locals[id as usize].declared;
                        if existing != declared && existing != Ty::ERROR && declared != Ty::ERROR {
                            let span = self.spans.token(*offset as usize);
                            let first = self.types.display(existing);
                            let offset_first = self.frame.locals[id as usize].offset;
                            self.report(
                                Diagnostic::error(
                                    Code::LOCAL_TYPE_CHANGED,
                                    span,
                                    text!(self,
                                        "`{name}` is already declared as {first}; a local keeps the type of its first declaration"
                                    ),
                                )
                                .with_label(self.spans.token(offset_first), "declared here")
                                .with_types(first, self.types.display(declared)),
                            );
                        }
                        id
                    }
                    None => {
                        let Some(id) = self.declare(name, declared, *offset as usize, true) else {
                            return ty;
                        };
                        self.frame.locals[id as usize].checked = true;
                        id
                    }
                };
                self.assign_local(id, ty);
                ty
            }
            Target::Value(expr) => match &expr.node {
                Node::Var(name) if name.starts_with("@@") => {
                    let key = (self.frame.owner, self.copy(name));
                    match self.constants.get(&key).copied() {
                        Some(declared) if self.frame.owner.is_some() => {
                            self.symbols(Some(CLASS_SYMBOL), |this| {
                                this.expr_against(
                                    value,
                                    declared,
                                    &Purpose::Ivar(this.copy(&name[1..])),
                                )
                            })
                        }
                        _ => {
                            let ty = self.expr(value, None);
                            self.write_class_variable(name, ty, expr, value);
                            ty
                        }
                    }
                }
                Node::Var(name) if name.starts_with('@') => {
                    let span = self.spans.expr(expr);
                    let ivar = &name[1..];
                    let expected = self.ivar_type(ivar, span);
                    if matches!(&value.node, Node::Var(value) if value.as_str() == "self") {
                        self.storing_self = Some(self.copy(ivar));
                    }
                    let ty = match expected {
                        Some(expected) => self.symbols(None, |this| {
                            this.expr_against(value, expected, &Purpose::Ivar(this.copy(ivar)))
                        }),
                        None => self.expr(value, None),
                    };
                    self.mark_ivar_assigned(ivar);
                    ty
                }
                Node::Var(name) if self.frame.namespace_body && is_constant(name) => {
                    if let Some(declared) = self.declared_constant(name) {
                        return self.symbols(None, |this| {
                            this.expr_against(value, declared, &Purpose::Local(this.copy(name)))
                        });
                    }
                    let key = (self.frame.owner, self.copy(name));
                    if let Some(&declared) = self.constants.get(&key) {
                        self.symbols(Some(LOCAL_SYMBOL), |this| {
                            this.expr_against(value, declared, &Purpose::Local(this.copy(name)))
                        })
                    } else {
                        let ty = self.expr(value, None);
                        if self.keep_constant(key, ty) {
                            return Ty::ERROR;
                        }
                        ty
                    }
                }
                Node::Var(name) => {
                    if name.as_str() == "self" {
                        return self.expr(value, None);
                    }
                    match self.local(name) {
                        Some(id) => {
                            let local = &self.frame.locals[id as usize];
                            let declared = local.declared;
                            // The runtime checks what a declared local holds.
                            let stay = (!local.checked).then_some(LOCAL_SYMBOL);
                            let ty = self.symbols(stay, |this| this.expr(value, Some(declared)));
                            if !self.types.assignable(ty, declared) {
                                let span = self.spans.expr(value);
                                self.local_changed(id, span, ty);
                            }
                            self.assign_local(id, ty);
                            ty
                        }
                        None if self.host_global(name).is_some() => {
                            let global = self.host_global(name).unwrap();
                            self.expr_against(value, global, &Purpose::Global(self.copy(name)))
                        }
                        None => {
                            let ty = self.expr(value, None);
                            let declared = self.local_type(name, ty, value, expr.offset as usize);
                            let Some(id) =
                                self.declare(name, declared, expr.offset as usize, false)
                            else {
                                return ty;
                            };
                            if let (Node::Hash(_), Kind::Shape(..)) =
                                (&value.node, self.types.kind(ty))
                            {
                                let uniform = self.uniform_field_type(ty);
                                self.frame.locals[id as usize].dictionary = uniform;
                            }
                            self.assign_local(id, ty);
                            ty
                        }
                    }
                }
                Node::Index(receiver, selectors) => {
                    self.index_write(expr, receiver, selectors, Ty::ERROR, value, true)
                }
                Node::Member(receiver, name) => {
                    self.setter(expr, receiver, name, Ty::ERROR, value, true)
                }
                Node::Scope(..) => self.expr(value, None),
                _ => self.expr(value, None),
            },
            Target::Tuple(_) => {
                let ty = self.destructured(target, value);
                self.bind_target(target, ty, true);
                ty
            }
        }
    }

    /// The type of a value destructured into `target`: an array literal is
    /// a tuple of its items, and so is an item that a nested pattern
    /// destructures in turn, as in `a, (b, c) = [1, [2, 3]]`.
    fn destructured(&mut self, target: &'a Target, value: &'a Expr) -> Ty {
        let (Target::Tuple(parts), Node::Array(items)) = (target, &value.node) else {
            return self.expr(value, None);
        };
        let splat = parts.iter().position(|(_, rest)| *rest);
        let after = splat.map_or(0, |splat| parts.len() - splat - 1);
        let Some(held) = self.hold(items.len() * std::mem::size_of::<Ty>()) else {
            return Ty::ERROR;
        };
        let types: Vec<Ty> = items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                // The part this item binds: before a splat by position, after
                // it from the end.
                let part = match splat {
                    Some(splat) if index >= splat => {
                        let from_end = items.len() - index;
                        (from_end <= after).then(|| parts.len() - from_end)
                    }
                    _ => Some(index),
                };
                match part
                    .and_then(|part| parts.get(part))
                    .and_then(|(part, _)| part.as_ref())
                {
                    Some(nested @ Target::Tuple(_)) => self.destructured(nested, item),
                    _ => self.expr(item, None),
                }
            })
            .collect();
        let tuple = self.types.tuple(types);
        self.release(held);
        tuple
    }

    /// The type a local first assigned a value of type `ty` gets; `nil`,
    /// `[]` and `{}` need a declared type.
    fn local_type(&mut self, name: &str, ty: Ty, value: &Expr, offset: usize) -> Ty {
        if ty == Ty::ERROR {
            return Ty::ERROR;
        }
        if literal_without_type(value) && self.needs_context(ty) {
            let span = self.spans.token(offset);
            let what = match self.types.kind(ty) {
                Kind::Nil => "`nil`",
                Kind::EmptyHash => "`{}`",
                _ => "an empty array",
            };
            self.report(Diagnostic::error(
                Code::NEEDS_TYPE,
                span,
                text!(
                    self,
                    "{what} does not say what `{name}` holds; declare it, as in `{name}: T = ...`"
                ),
            ));
            return Ty::ERROR;
        }
        ty
    }

    /// Whether a value's type leaves out what a local would hold.
    pub(super) fn needs_context(&self, ty: Ty) -> bool {
        match self.types.kind(ty) {
            Kind::Nil | Kind::EmptyHash | Kind::Never => true,
            Kind::Array(element) => *element == Ty::NEVER || self.needs_context_inner(*element),
            Kind::Hash(value) => *value == Ty::NEVER,
            _ => false,
        }
    }

    fn needs_context_inner(&self, ty: Ty) -> bool {
        match self.types.kind(ty) {
            Kind::Array(element) => *element == Ty::NEVER || self.needs_context_inner(*element),
            Kind::EmptyHash => true,
            _ => false,
        }
    }

    fn uniform_field_type(&mut self, shape: Ty) -> Option<Ty> {
        let Kind::Shape(fields, false) = &*self.types.shared(shape) else {
            return None;
        };
        let first = fields.first()?.ty;
        fields
            .iter()
            .all(|f| f.ty == first && !f.optional)
            .then_some(first)
    }

    /// Reports an assignment that changes a local's type.
    pub(super) fn local_changed(&mut self, id: LocalId, span: Span, ty: Ty) {
        if ty == Ty::ERROR {
            return;
        }
        let local = &self.frame.locals[id as usize];
        let declared = local.declared;
        if declared == Ty::ERROR {
            return;
        }
        let name = local.name.clone();
        let first = self.spans.token(local.offset);
        let expected = self.types.display(declared);
        let found = self.types.display(ty);
        let how = if local.annotated {
            "declared"
        } else {
            "fixed by its first assignment"
        };
        self.report(
            Diagnostic::error(
                Code::LOCAL_TYPE_CHANGED,
                span,
                text!(
                    self,
                    "`{name}` is {expected}, {how}; it cannot hold {found}"
                ),
            )
            .with_label(first, "declared here")
            .with_types(expected, found),
        );
    }

    /// Binds a destructuring or block parameter target to a value of type `ty`.
    pub(super) fn bind_target(&mut self, target: &'a Target, ty: Ty, assignment: bool) {
        if assignment {
            self.check_binding_target(target);
        }
        match target {
            Target::Value(expr) => match &expr.node {
                Node::Var(name) if assignment && self.host_global(name).is_some() => {
                    let global = self.host_global(name).unwrap();
                    let span = self.spans.expr(expr);
                    self.global_write(name, global, ty, span);
                }
                Node::Var(name) if !name.starts_with('@') => {
                    let id = match (assignment, self.local(name)) {
                        (true, Some(id)) => {
                            let declared = self.frame.locals[id as usize].declared;
                            if !self.types.assignable(ty, declared) {
                                let span = self.spans.expr(expr);
                                self.local_changed(id, span, ty);
                            }
                            id
                        }
                        // A part such as the missing second element of
                        // `a, b = [1]` is `nil`, and uses of it are checked
                        // as any other type's.
                        _ => {
                            let Some(id) = self.declare(name, ty, expr.offset as usize, false)
                            else {
                                return;
                            };
                            id
                        }
                    };
                    self.assign_local(id, ty);
                }
                Node::Var(name) => {
                    let span = self.spans.expr(expr);
                    if let Some(expected) = self.ivar_type(&name[1..], span) {
                        if !self.types.assignable(ty, expected) {
                            self.mismatch(
                                span,
                                expected,
                                ty,
                                &Purpose::Ivar(self.copy(&name[1..])),
                            );
                        }
                    }
                    self.mark_ivar_assigned(&name[1..]);
                }
                // A destructured element written through an index or a
                // setter is checked as an assigned value is.
                Node::Index(receiver, selectors) if assignment => {
                    self.mark_write_chain(receiver);
                    // As a compound assignment does, the read checks the
                    // receiver and keys, and the write replays their types.
                    let outer = self.set_memo(Some(super::Memo::default()));
                    self.expr(expr, None);
                    self.memo.get_mut().unwrap().replay = true;
                    self.index_write(expr, receiver, selectors, ty, expr, false);
                    self.restore_memo(outer);
                }
                Node::Member(receiver, name) if assignment => {
                    self.setter(expr, receiver, name, ty, expr, false);
                }
                _ => {
                    self.expr(expr, None);
                }
            },
            Target::Typed(inner, annotation) => {
                let declared = self.annotation(
                    annotation,
                    self.frame.owner,
                    target.offset().unwrap_or(0) as usize,
                );
                if !self.types.assignable(ty, declared) {
                    let span = self.target_span(inner);
                    self.mismatch(span, declared, ty, &Purpose::Annotation);
                }
                match &**inner {
                    Target::Value(Expr {
                        node: Node::Var(name),
                        offset,
                        ..
                    }) if assignment && self.host_global(name).is_some() => {
                        // The value already has the declared type.
                        let global = self.host_global(name).unwrap();
                        let span = self.spans.token(*offset as usize);
                        self.global_declaration(name, global, declared, span);
                    }
                    Target::Value(Expr {
                        node: Node::Var(name),
                        offset,
                        ..
                    }) if !name.starts_with('@') => {
                        let id = match (assignment, self.local(name)) {
                            (true, Some(id)) => id,
                            _ => {
                                let Some(id) = self.declare(name, declared, *offset as usize, true)
                                else {
                                    return;
                                };
                                self.frame.locals[id as usize].checked = true;
                                id
                            }
                        };
                        self.assign_local(id, declared);
                    }
                    inner => self.bind_target(inner, declared, assignment),
                }
            }
            Target::Tuple(parts) => {
                let count = parts.len();
                let splat = parts.iter().position(|(_, rest)| *rest);
                for (index, (part, rest)) in parts.iter().enumerate() {
                    // A check its budget stops binds no more of them, and
                    // the budget is checked as a walk checks it.
                    if self.paced(index) {
                        return;
                    }
                    let Some(part) = part else {
                        continue;
                    };
                    let element = if *rest {
                        self.rest_of(ty, index, count)
                    } else {
                        self.part_of(ty, index, count, splat)
                    };
                    self.bind_target(part, element, assignment);
                }
            }
        }
    }

    fn check_binding_target(&mut self, target: &Target) {
        let Target::Value(expr) = target else {
            if let Target::Typed(inner, _) = target {
                self.check_binding_target(inner);
            }
            return;
        };
        let Node::Var(name) = &expr.node else { return };
        if is_constant(name) && !self.frame.main && !self.frame.namespace_body {
            self.report(Diagnostic::error(
                Code::LOCAL_TYPE_CHANGED,
                self.spans.expr(expr),
                text!(self, "a function cannot assign capitalized name `{name}`; use a lowercase local or a declared class variable"),
            ));
            return;
        }
        if !is_constant(name) || self.local(name).is_some() {
            return;
        }
        let reserved = self.program.roots.contains_key(name.as_str())
            || self.program.enum_names.contains_key(name.as_str())
            || self.program.functions.contains_key(name.as_str())
            || self.program.hosts.contains_key(name.as_str())
            || super::sigs::index().module(name).is_some()
            || super::sigs::index().globals.contains_key(name.as_str());
        if reserved {
            self.report(Diagnostic::error(
                Code::LOCAL_TYPE_CHANGED,
                self.spans.expr(expr),
                text!(
                    self,
                    "`{name}` names a namespace or function and cannot be rebound"
                ),
            ));
        }
    }

    /// The type of element `index` when destructuring a value of type `ty`.
    pub(super) fn element_of(&mut self, ty: Ty, index: usize) -> Ty {
        match &*self.types.shared(ty) {
            Kind::Tuple(items) => items.get(index).copied().unwrap_or(Ty::NIL),
            Kind::Array(element) => self.types.optional(*element),
            Kind::Error | Kind::Any => ty,
            _ if index == 0 => ty,
            _ => Ty::NIL,
        }
    }

    /// The type of target `index` of `count` when destructuring a value of
    /// type `ty`, with a splat target at `splat`. A target after the splat
    /// takes an element from the end, but never one a target before the
    /// splat took: `a, *m, y = [1]` leaves `y` nil.
    fn part_of(&mut self, ty: Ty, index: usize, count: usize, splat: Option<usize>) -> Ty {
        match (splat, self.types.kind(ty)) {
            (Some(splat), Kind::Tuple(items)) if index > splat => {
                let after = count - splat - 1;
                let start = splat.max(items.len().saturating_sub(after));
                items
                    .get(start + index - splat - 1)
                    .copied()
                    .unwrap_or(Ty::NIL)
            }
            _ => self.element_of(ty, index),
        }
    }

    fn rest_of(&mut self, ty: Ty, index: usize, count: usize) -> Ty {
        match &*self.types.shared(ty) {
            Kind::Tuple(items) => {
                let end = items.len().saturating_sub(count - index - 1).max(index);
                let slice: Vec<Ty> = items
                    .get(index..end)
                    .map(<[Ty]>::to_vec)
                    .unwrap_or_default();
                let element = self.types.union(&slice);
                self.types.array(element)
            }
            Kind::Array(_) => ty,
            Kind::Error | Kind::Any => ty,
            _ => self.types.array(ty),
        }
    }

    // Instance variables -----------------------------------------------

    /// The declared type of an instance variable of the current class,
    /// reporting an undeclared one.
    pub(super) fn ivar_type(&mut self, name: &str, span: Span) -> Option<Ty> {
        let Some(ns) = self.frame.owner else {
            self.report(Diagnostic::error(
                Code::UNDECLARED_IVAR,
                span,
                text!(
                    self,
                    "`@{name}` is outside any class; instance variables belong to a class"
                ),
            ));
            return None;
        };
        let namespace = &self.program.namespaces[ns as usize];
        if let Some(ivar) = namespace.ivars.get(name) {
            return Some(ivar.ty);
        }
        let class = namespace.name.clone();
        self.report(Diagnostic::error(
            Code::UNDECLARED_IVAR,
            span,
            text!(self,
                "`@{name}` is not declared in `{class}`; declare it in the class body, as in `@{name}: T`"
            ),
        ));
        None
    }

    /// The type the current class or module body declares for constant
    /// `name`, as in `LIMIT: int = 3`, which every assignment keeps.
    fn declared_constant(&mut self, name: &str) -> Option<Ty> {
        let ns = self.frame.owner?;
        let module = self.program.namespaces[ns as usize].module?;
        let (ty, offset) = module.body.iter().find_map(|stmt| match &stmt.node {
            Statement::Assign(target, "=", _) => {
                let (declared, ty) = crate::syntax::typed::declared_local(target)?;
                (declared == name).then_some((ty, stmt.offset))
            }
            _ => None,
        })?;
        Some(self.annotation(ty, Some(ns), offset as usize))
    }

    /// Assigns a class variable, which its class or module body declares
    /// as `@@name: T = value`; every assignment must keep that type.
    fn write_class_variable(&mut self, name: &str, ty: Ty, target: &Expr, value: &Expr) {
        let Some(ns) = self.frame.owner else {
            self.report(Diagnostic::error(
                Code::UNDECLARED_IVAR,
                self.spans.expr(target),
                text!(self, "class variable `{name}` is outside a class"),
            ));
            return;
        };
        let key = (Some(ns), self.copy(name));
        if let Some(declared) = self.constants.get(&key).copied() {
            if !self.types.assignable(ty, declared) {
                let span = self.spans.expr(value);
                self.mismatch(span, declared, ty, &Purpose::Ivar(self.copy(&name[1..])));
            }
            return;
        }
        let span = self.spans.expr(target);
        let class = self.program.namespaces[ns as usize].name.clone();
        let mut diagnostic = Diagnostic::error(
            Code::UNDECLARED_IVAR,
            span,
            text!(
                self,
                "class variable `{name}` is not declared in `{class}`; declare it in the body, as in `{name}: T = value`"
            ),
        );
        let nameable = !self.needs_context(ty)
            && !matches!(self.types.kind(ty), Kind::Error | Kind::Never | Kind::Any);
        if nameable && self.frame.namespace_body && self.class_body_assignment(ns, target) {
            let written = self.types.display(ty);
            diagnostic = diagnostic.with_fix(Fix::insert(
                text!(self, "declare `{name}: {written}`"),
                span.end,
                text!(self, ": {written}"),
            ));
        }
        self.report(diagnostic);
        // Later reads and writes check against the first value's type.
        let ty = if nameable { ty } else { Ty::ERROR };
        if self.keep_constant(key, ty) {
            self.stopped = true;
        }
    }

    /// Records constant `key` of type `ty`. A new one's name, and its room
    /// in the table, are counted before it is kept, and a check they stop
    /// keeps no more constants. Returns whether the check has stopped.
    #[must_use = "the budget may have stopped the check, which must then do no more work"]
    pub(super) fn keep_constant(&mut self, key: (Option<NsId>, String), ty: Ty) -> bool {
        if let Some(entry) = self.constants.get_mut(&key) {
            *entry = ty;
            return self.halted();
        }
        let bytes = key.1.capacity();
        if self.constants.insert(self.meter.tables(), key, ty).is_err() {
            return true;
        }
        self.grown += bytes;
        self.halted()
    }

    /// Whether `target` is the target of a plain assignment that stands
    /// directly in the body of namespace `ns`, where a declaration can
    /// replace it.
    fn class_body_assignment(&self, ns: NsId, target: &Expr) -> bool {
        let Some(module) = self.program.namespaces[ns as usize].module else {
            return false;
        };
        module.body.iter().any(|stmt| match &stmt.node {
            Statement::Assign(Target::Value(assigned), "=", _) => {
                assigned.offset == target.offset && stmt.offset == target.offset
            }
            _ => false,
        })
    }

    pub(super) fn mark_ivar_assigned(&mut self, name: &str) {
        let Some((roster, first)) = &self.frame.initialize else {
            return;
        };
        if let Ok(place) = roster.binary_search_by(|ivar| ivar.as_str().cmp(name)) {
            let id = first + place as LocalId;
            let state = self.frame.flow.get(id);
            self.frame.flow.set(
                id,
                VarState {
                    assigned: true,
                    ..state
                },
            );
        }
    }

    /// Assigns a parameter to an instance variable, as `def initialize(@name)` does.
    fn assign_ivar(&mut self, name: &str, ty: Ty, span: Span, accessor: bool) {
        if !accessor {
            if let Some(expected) = self.ivar_type(name, span) {
                if !self.types.assignable(ty, expected) {
                    self.mismatch(span, expected, ty, &Purpose::Ivar(self.copy(name)));
                }
            }
        }
        self.mark_ivar_assigned(name);
    }

    fn write_ivar(&mut self, name: &str, ty: Ty, span: Span, value: &Expr) {
        if let Some(expected) = self.ivar_type(name, span) {
            if !self.types.assignable(ty, expected) {
                let span = self.spans.expr(value);
                self.mismatch(span, expected, ty, &Purpose::Ivar(self.copy(name)));
            }
        }
        self.mark_ivar_assigned(name);
    }

    // Conditions and narrowing -----------------------------------------

    /// Checks a condition, which must be a `bool`, and the narrowings it implies.
    pub(super) fn condition(&mut self, expr: &'a Expr) -> Narrow {
        let (ty, narrow) = self.condition_parts(expr);
        if ty != Ty::BOOL && ty != Ty::ERROR && ty != Ty::NEVER {
            let span = self.spans.expr(expr);
            let found = self.types.display(ty);
            let mut diagnostic = Diagnostic::error(
                Code::CONDITION_NOT_BOOL,
                span,
                text!(self, "a condition must be a bool, found {found}"),
            )
            .with_types("bool", found.clone());
            // A nil test keeps an optional value's meaning, unless it may be false.
            let optional = self.types.has_nil(ty) && ty != Ty::NIL;
            let rest = self.types.without_nil(ty);
            if optional
                && !self.types.assignable(Ty::BOOL, rest)
                && !self.types.members(rest).contains(&Ty::BOOL)
            {
                let text = &self.source[span.start..span.end];
                let replacement = if is_simple(expr) {
                    text!(self, "{text} != nil")
                } else {
                    text!(self, "({text}) != nil")
                };
                diagnostic = diagnostic.with_fix(Fix::replace(
                    text!(self, "test for nil: `{replacement}`"),
                    span,
                    replacement,
                ));
            }
            self.report(diagnostic);
        }
        narrow
    }

    /// The type of a condition expression and what it narrows.
    fn condition_parts(&mut self, expr: &'a Expr) -> (Ty, Narrow) {
        match &expr.node {
            Node::Unary("!", inner) => {
                let (ty, narrow) = self.condition_parts(inner);
                self.require_bool(inner, ty, "!");
                (
                    Ty::BOOL,
                    Narrow {
                        then: narrow.otherwise,
                        otherwise: narrow.then,
                    },
                )
            }
            Node::Binary("&&", left, right) => {
                let (lt, ln) = self.condition_parts(left);
                self.require_bool(left, lt, "&&");
                let mark = self.frame.flow.mark();
                self.apply(&ln.then);
                let (rt, rn) = self.condition_parts(right);
                self.require_bool(right, rt, "&&");
                self.frame.flow.rollback(mark);
                let merged = merge(&self.meter, &ln.then, &rn.otherwise);
                let otherwise = self.join_narrowings(&ln.otherwise, &merged);
                drop(merged);
                // The left's list takes the right's, its growth counted
                // first; a refusal stops the check.
                let mut then = ln.then;
                if then.extend_from_slice(&rn.then).is_err() {
                    return (Ty::ERROR, Narrow::new(&self.meter));
                }
                (Ty::BOOL, Narrow { then, otherwise })
            }
            Node::Binary("||", left, right) => {
                let (lt, ln) = self.condition_parts(left);
                self.require_bool(left, lt, "||");
                let mark = self.frame.flow.mark();
                self.apply(&ln.otherwise);
                let (rt, rn) = self.condition_parts(right);
                self.require_bool(right, rt, "||");
                self.frame.flow.rollback(mark);
                let merged = merge(&self.meter, &ln.otherwise, &rn.then);
                let then = self.join_narrowings(&ln.then, &merged);
                drop(merged);
                let mut otherwise = ln.otherwise;
                if otherwise.extend_from_slice(&rn.otherwise).is_err() {
                    return (Ty::ERROR, Narrow::new(&self.meter));
                }
                (Ty::BOOL, Narrow { then, otherwise })
            }
            Node::Binary(op @ ("==" | "!="), left, right) => {
                let ty = self.expr(expr, None);
                let subject = match (&left.node, &right.node) {
                    (_, Node::Literal(v)) if v.type_name() == "nil" => Some(&**left),
                    (Node::Literal(v), _) if v.type_name() == "nil" => Some(&**right),
                    _ => None,
                };
                let mut narrow = Narrow::new(&self.meter);
                if let Some(id) = subject.and_then(|subject| self.narrowable(subject)) {
                    let current = self.frame.flow.get(id).ty;
                    let without = self.types.without_nil(current);
                    let optional = self.types.has_nil(current) || current == Ty::ANY;
                    if !optional && current != Ty::ERROR && current != Ty::NEVER {
                        let span = self.spans.expr(expr);
                        let name = self.frame.locals[id as usize].name.clone();
                        let found = self.types.display(current);
                        let always = if *op == "==" { "false" } else { "true" };
                        self.report(Diagnostic::warning(
                            Code::UNREACHABLE_NARROWING,
                            span,
                            text!(
                                self,
                                "`{name}` is {found}, never nil, so this test is always {always}"
                            ),
                        ));
                    }
                    let nil = if optional { Ty::NIL } else { current };
                    if *op == "==" {
                        narrow.then.add((id, nil));
                        narrow.otherwise.add((id, without));
                    } else {
                        narrow.then.add((id, without));
                        narrow.otherwise.add((id, nil));
                    }
                }
                (ty, narrow)
            }
            Node::Method(receiver, name, args, _)
                if name.as_str() == "is_type?" && args.len() == 1 =>
            {
                let ty = self.expr(expr, None);
                let mut narrow = Narrow::new(&self.meter);
                if let (Some(id), Some(tested)) =
                    (self.narrowable(receiver), self.type_atom(&args[0].value))
                {
                    let current = self.frame.flow.get(id).ty;
                    let then = self.intersect(current, tested);
                    if then == Ty::NEVER && current != Ty::NEVER {
                        let span = self.spans.expr(expr);
                        let found = self.types.display(current);
                        let wanted = self.types.display(tested);
                        self.report(Diagnostic::warning(
                            Code::CAST,
                            span,
                            text!(self, "a value of type {found} is never {wanted}, so this test is always false"),
                        ));
                    }
                    let otherwise = if current == Ty::ANY {
                        Ty::ANY
                    } else {
                        self.types.without(current, tested)
                    };
                    narrow.then.add((id, then));
                    narrow.otherwise.add((id, otherwise));
                }
                (ty, narrow)
            }
            Node::Var(name) if name.as_str() == "block_given?" && self.local(name).is_none() => {
                let mut narrow = Narrow::new(&self.meter);
                if let Some(id) = self.frame.block_given {
                    narrow.then.add((id, Ty::BOOL));
                }
                (Ty::BOOL, narrow)
            }
            _ => (self.expr(expr, None), Narrow::new(&self.meter)),
        }
    }

    fn require_bool(&mut self, expr: &Expr, ty: Ty, op: &str) {
        if ty == Ty::BOOL || ty == Ty::ERROR || ty == Ty::NEVER {
            return;
        }
        let span = self.spans.expr(expr);
        let found = self.types.display(ty);
        self.report(
            Diagnostic::error(
                Code::LOGICAL_NOT_BOOL,
                span,
                text!(self, "`{op}` takes bool operands, found {found}"),
            )
            .with_types("bool", found),
        );
    }

    /// Narrowings that hold on either of two paths.
    fn join_narrowings(
        &mut self,
        a: &[(LocalId, Ty)],
        b: &[(LocalId, Ty)],
    ) -> ScratchVec<(LocalId, Ty)> {
        // Each narrowing of one is compared with the other's, which is
        // charged first, and the joined ones are kept in a list counted
        // while it lives.
        let mut joined = ScratchVec::new(&self.meter);
        if self.types.work(a.len().saturating_mul(b.len())) {
            return joined;
        }
        for &(id, ty) in a {
            if let Some(&(_, other)) = b.iter().rev().find(|(other, _)| *other == id) {
                joined.add((id, self.types.union(&[ty, other])));
            }
        }
        joined
    }

    /// The local an expression reads, if narrowing may apply to it.
    pub(super) fn narrowable(&self, expr: &Expr) -> Option<LocalId> {
        match &expr.node {
            Node::Var(name) => {
                let id = self.local(name)?;
                self.frame.flow.get(id).assigned.then_some(id)
            }
            _ => None,
        }
    }

    /// The type an `is_type?` atom such as `:int` tests for.
    pub(super) fn type_atom(&mut self, atom: &Expr) -> Option<Ty> {
        let Node::Literal(value) = &atom.node else {
            return None;
        };
        let name = super::symbol_text(value)?;
        let name = name.as_str();
        let (name, nullable) = match name.strip_suffix('?') {
            Some(base) => (base, true),
            None => (name, false),
        };
        let ty = match name {
            "nil" => Ty::NIL,
            "bool" => Ty::BOOL,
            "int" => Ty::INT,
            "float" => Ty::FLOAT,
            "number" => Ty::NUMBER,
            "string" => Ty::STRING,
            "symbol" => Ty::SYMBOL,
            "array" => self.types.array(Ty::ANY),
            "hash" | "object" => self.types.hash(Ty::ANY),
            "range" => Ty::RANGE,
            "duration" => Ty::DURATION,
            "time" => Ty::TIME,
            "money" => Ty::MONEY,
            other => {
                let base = other.rsplit('.').next().unwrap_or(other);
                if let Some(&id) = self.program.enum_names.get(base) {
                    self.types.intern(Kind::EnumValue(id))
                } else {
                    // A pass over the namespaces, a step for each 64.
                    if self.types.work(self.program.namespaces.len()) {
                        return None;
                    }
                    let ns = self.program.namespaces.iter().position(|ns| {
                        ns.is_class && ns.module.is_some_and(|module| module.name == base)
                    })?;
                    self.types.intern(Kind::Instance(ns as NsId))
                }
            }
        };
        Some(if nullable {
            self.types.optional(ty)
        } else {
            ty
        })
    }

    /// The part of `current` that `tested` describes: the alternatives it
    /// covers, or the tested type itself when `current` is `any`.
    fn intersect(&mut self, current: Ty, tested: Ty) -> Ty {
        if current == Ty::ANY || current == Ty::ERROR {
            return tested;
        }
        // A member matches a tested alternative it fits, or one of the same
        // base, which the tested alternatives' bases decide at once.
        let tested_members = self.types.members(tested);
        let arrays = tested_members
            .iter()
            .any(|&t| same_base(&Kind::Array(Ty::ANY), self.types.kind(t)));
        let hashes = tested_members
            .iter()
            .any(|&t| same_base(&Kind::EmptyHash, self.types.kind(t)));
        let mut kept = Vec::new();
        for member in self.types.members(current) {
            let base = match self.types.kind(member) {
                Kind::Array(_) | Kind::Tuple(_) => arrays,
                Kind::Hash(_) | Kind::Shape(..) | Kind::EmptyHash => hashes,
                _ => false,
            };
            if base || self.types.assignable(member, tested) {
                kept.push(member);
            }
        }
        if kept.is_empty() {
            Ty::NEVER
        } else {
            self.types.union(&kept)
        }
    }

    /// The function a frame is checking, by id, for messages.
    pub(super) fn current_function(&self) -> &str {
        &self.frame.name
    }
}

/// Whether an expression is `nil`, `[]` or `{}`, or an array of empty
/// arrays, whose type says nothing about what it will hold.
fn literal_without_type(expr: &Expr) -> bool {
    match &expr.node {
        Node::Literal(value) => value.type_name() == "nil",
        Node::Hash(entries) => entries.is_empty(),
        Node::Array(items) => items.iter().all(literal_without_type),
        _ => false,
    }
}

/// The narrowings of `a` and then `b`, in a list counted while it lives;
/// none once the budget refuses it room, which stops the check.
fn merge(
    meter: &std::sync::Arc<super::meter::Meter>,
    a: &[(LocalId, Ty)],
    b: &[(LocalId, Ty)],
) -> ScratchVec<(LocalId, Ty)> {
    let mut merged = ScratchVec::new(meter);
    if merged.reserve(a.len() + b.len()).is_err() {
        return merged;
    }
    for &narrowing in a.iter().chain(b) {
        merged.add(narrowing);
    }
    merged
}

fn same_base(a: &Kind, b: &Kind) -> bool {
    matches!(
        (a, b),
        (Kind::Array(_) | Kind::Tuple(_), Kind::Array(_))
            | (
                Kind::Hash(_) | Kind::Shape(..) | Kind::EmptyHash,
                Kind::Hash(_)
            )
    )
}

pub(super) fn is_constant(name: &str) -> bool {
    name.chars().next().is_some_and(char::is_uppercase)
}

fn is_simple(expr: &Expr) -> bool {
    matches!(
        expr.node,
        Node::Var(_) | Node::Member(..) | Node::Method(..) | Node::Index(..) | Node::Call(..)
    )
}

fn target_expr(target: &Target) -> Option<&Expr> {
    match target {
        Target::Value(expr) => Some(expr),
        Target::Typed(inner, _) => target_expr(inner),
        Target::Tuple(_) => None,
    }
}

/// Adds the names `body` mentions as variables, assignment targets or bare
/// calls, which are the enclosing locals a namespace body can read or
/// update, however deep in its expressions, charging the walk to `meter`.
/// Returns the bytes of the stack the walk kept.
fn mentions<'s>(
    meter: &super::meter::Meter,
    body: &'s [Stmt],
    names: &mut super::counted::ScratchSet<&'s str>,
) -> usize {
    use super::walk::{Item, Walk};
    let mut walk = Walk::new(meter);
    // The body is a visit, empty or not; the names count themselves.
    walk.visit(0);
    walk.stmts(body, ());
    while let Some((item, ())) = walk.next(0) {
        if let Item::Expr(Expr {
            node: Node::Var(name) | Node::Call(name, _, _),
            ..
        }) = item
        {
            // A name the budget refuses room for stops the check, which
            // the walk reads.
            if names.insert(name.as_str()).is_err() {
                break;
            }
        }
        walk.children(item, ());
    }
    walk.bytes()
}

/// The names of the locals a body may assign, including in nested blocks,
/// charging the walk that finds them to `meter`.
/// Returns the bytes of the index it built to find them.
pub(super) fn assigned_names(
    meter: &std::sync::Arc<super::meter::Meter>,
    body: &[Stmt],
    names: &mut ScratchVec<String>,
) -> usize {
    let mut assigns = super::assigns::Assigns::default();
    let span = assigns.body(meter, body);
    // The list of the names is counted before it is made; their copies are
    // counted, with the list they go in, as it takes them.
    let held = assigns.bytes();
    let found = assigns.distinct(span, |count, _| {
        !meter.pace(0, held + count * std::mem::size_of::<&str>())
    });
    // The copies are counted, with room for them, before they are made.
    let bytes = found.iter().map(|name| name.len()).sum();
    if names.reserve_with(found.len(), bytes).is_ok() {
        for name in found {
            names.push_within(name.to_owned());
        }
    }
    assigns.bytes()
}

/// Why a value is checked against a type, for messages.
#[derive(Clone)]
pub(crate) enum Purpose {
    Result,
    BlockResult,
    Local(String),
    /// A global the host declares, which a write updates.
    Global(String),
    Ivar(String),
    Argument {
        index: usize,
        name: String,
        function: String,
    },
    Keyword {
        name: String,
        function: String,
    },
    Element,
    Field(String),
    Annotation,
    Yield(usize),
    Operand,
    /// A `break` out of a block, which returns from this function, or ends
    /// a loop or block around its `yield`.
    Break(String, bool),
}

impl Heap for Purpose {
    fn heap(&self) -> usize {
        match self {
            Purpose::Local(name)
            | Purpose::Global(name)
            | Purpose::Ivar(name)
            | Purpose::Field(name)
            | Purpose::Break(name, _) => name.heap(),
            Purpose::Argument { name, function, .. } | Purpose::Keyword { name, function } => {
                name.heap() + function.heap()
            }
            Purpose::Result
            | Purpose::BlockResult
            | Purpose::Element
            | Purpose::Annotation
            | Purpose::Yield(_)
            | Purpose::Operand => 0,
        }
    }
}

impl<'a> Checker<'a> {
    /// What a position expects, for messages: "`f` returns int".
    pub(super) fn purpose_text(&self, purpose: &Purpose, expected_text: &str) -> String {
        match purpose {
            Purpose::Result => text!(
                self,
                "`{}` returns {expected_text}",
                self.current_function()
            ),
            Purpose::BlockResult => text!(self, "the block returns {expected_text}"),
            Purpose::Local(name) => text!(self, "`{name}` is {expected_text}"),
            Purpose::Global(name) => text!(
                self,
                "the host declares the global `{name}` as {expected_text}, and this writes it"
            ),
            Purpose::Ivar(name) => text!(self, "`@{name}` is {expected_text}"),
            Purpose::Argument {
                index,
                name,
                function,
            } => {
                text!(
                    self,
                    "argument {} (`{name}`) of `{function}` is {expected_text}",
                    index + 1
                )
            }
            Purpose::Keyword { name, function } => {
                text!(self, "keyword `{name}:` of `{function}` is {expected_text}")
            }
            Purpose::Element => text!(self, "elements here are {expected_text}"),
            Purpose::Field(name) => text!(self, "field `{name}` is {expected_text}"),
            Purpose::Annotation => text!(self, "the annotation says {expected_text}"),
            Purpose::Yield(index) => text!(self, "block argument {} is {expected_text}", index + 1),
            Purpose::Operand => text!(self, "the operand must be {expected_text}"),
            Purpose::Break(function, false) => text!(
                self,
                "a `break` out of the block returns from `{function}`, which returns {expected_text}"
            ),
            Purpose::Break(function, true) => text!(
                self,
                "a `break` value out of this block ends a loop or block inside `{function}`, which takes it as a value of its result type, {expected_text}"
            ),
        }
    }

    /// Reports that a value of type `found` is not assignable to `expected`.
    pub(super) fn mismatch(&mut self, span: Span, expected: Ty, found: Ty, purpose: &Purpose) {
        // A stopped check builds no more findings, which it would drop.
        if expected == Ty::ERROR || found == Ty::ERROR || self.halted() {
            return;
        }
        let expected_text = self.types.display(expected);
        let found_text = self.types.display(found);
        let what = self.purpose_text(purpose, &expected_text);
        let mut diagnostic = Diagnostic::error(
            Code::TYPE_MISMATCH,
            span,
            text!(self, "{what}, found {found_text}"),
        )
        .with_types(expected_text.clone(), found_text);
        if found == Ty::ANY {
            diagnostic.code = Code::ANY_USE;
            diagnostic.message = text!(
                self,
                "{what}, found any; narrow the value first with `is_type?`, `.as({expected_text})` or `JSON.parse_as`"
            );
        } else if self.types.has_nil(found) {
            let without = self.types.without_nil(found);
            if without != Ty::NEVER && self.types.assignable(without, expected) {
                diagnostic.code = Code::OPTIONAL_USE;
                diagnostic.message = text!(
                    self,
                    "{what}, but this value may be nil; test it with `!= nil` first"
                );
            }
        }
        self.report(diagnostic);
    }
}
