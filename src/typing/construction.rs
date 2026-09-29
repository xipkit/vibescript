//! Reports reads of instance variables their instance has not assigned yet,
//! and proves the classes whose methods can never see one, so the runtime
//! need not check their instance method results.
//!
//! A required instance variable, one without a default whose type admits no
//! `nil`, reads as `nil` until `initialize` assigns it. While building an
//! instance, in `initialize` and in the instance variables' defaults, the
//! checker records each read of an unassigned variable, each call of one of
//! the class's methods on `self` and each other use of `self`. Once every
//! method is checked, each of these that can read an unassigned variable is
//! reported (V0205): a direct read, a call that reaches a method reading
//! one, directly or through the methods it calls on `self`, and a use of
//! `self` as a value while one is unassigned, since the value can then
//! reach any method. A class with required variables and no `initialize`
//! is not proven, and not reported: its methods keep their result check.

use super::{
    Checker,
    meter::{Heap, btree_entry},
    program::{FnId, NsId},
    ty::Ty,
};
use crate::{
    diagnostic::{Code, Diagnostic, Span},
    syntax::{Expr, Node},
};
use std::collections::{BTreeSet, HashMap, HashSet};

/// What the checker records about instance variable reads.
#[derive(Default)]
pub(crate) struct Construction {
    /// What each instance method does with `self`.
    methods: HashMap<FnId, Uses>,
    /// Uses of `self` while it has unassigned variables.
    sites: Vec<Site>,
    /// What the methods' uses and the sites hold beyond the tables' own
    /// storage, for the checker's memory account.
    held: usize,
}

impl Construction {
    /// What the records hold.
    pub fn bytes(&self) -> usize {
        super::meter::map(&self.methods) + super::meter::vec(&self.sites) + self.held
    }
}

/// What a method does with `self`, directly or in its blocks.
#[derive(Default)]
struct Uses {
    reads: BTreeSet<String>,
    calls: BTreeSet<FnId>,
    /// Whether `self` is used as a value, which lets any method read it.
    escapes: bool,
}

/// A use of `self` while it has unassigned variables.
struct Site {
    class: NsId,
    kind: SiteKind,
    unassigned: Vec<String>,
    span: Span,
}

enum SiteKind {
    Read(String),
    Call(FnId),
    Escape,
}

impl Heap for SiteKind {
    fn heap(&self) -> usize {
        match self {
            SiteKind::Read(name) => name.heap(),
            SiteKind::Call(_) | SiteKind::Escape => 0,
        }
    }
}

impl<'a> Checker<'a> {
    /// The variables of the instance being built that are not assigned
    /// yet where the code being checked runs, if it builds one.
    fn unassigned(&self) -> Vec<String> {
        if !self.frame.flow.live {
            return Vec::new();
        }
        if let Some(building) = &self.frame.building {
            return building.clone();
        }
        self.frame
            .initialize
            .iter()
            .filter(|(_, id)| !self.frame.flow.get(*id).assigned)
            .map(|(name, _)| name.clone())
            .collect()
    }

    fn uses(&mut self) -> Option<&mut Uses> {
        let function = self.frame.function?;
        Some(self.construction.methods.entry(function).or_default())
    }

    fn site(&mut self, kind: SiteKind, span: Span) {
        let Some(class) = self.frame.owner else {
            return;
        };
        let unassigned = self.unassigned();
        if !unassigned.is_empty() {
            self.construction.held += unassigned.heap() + kind.heap();
            self.construction.sites.push(Site {
                class,
                kind,
                unassigned,
                span,
            });
        }
    }

    /// Records a read of instance variable `name` of `self` at `span`.
    pub(super) fn read_ivar(&mut self, name: &str, span: Span) {
        if let Some(uses) = self.uses() {
            let bytes = btree_entry(&uses.reads) + name.len();
            if uses.reads.insert(name.to_owned()) {
                self.construction.held += bytes;
            }
        }
        self.site(SiteKind::Read(name.to_owned()), span);
    }

    /// Records a call of method `callee` on `self` at `span`.
    pub(super) fn call_on_self(&mut self, callee: FnId, span: Span) {
        if let Some(uses) = self.uses() {
            let bytes = btree_entry(&uses.calls);
            if uses.calls.insert(callee) {
                self.construction.held += bytes;
            }
        }
        self.site(SiteKind::Call(callee), span);
    }

    /// Records a use of `self` as a value at `span`.
    pub(super) fn self_escapes(&mut self, span: Span) {
        // `@next = self` stores `self` in the variable it assigns, so it
        // escapes only while the others are unassigned.
        let stored = self.storing_self.take();
        if !self.frame.instance {
            return;
        }
        if self.self_receiver {
            self.self_receiver = false;
            return;
        }
        if let Some(uses) = self.uses() {
            uses.escapes = true;
        }
        let Some(class) = self.frame.owner else {
            return;
        };
        let mut unassigned = self.unassigned();
        unassigned.retain(|name| stored.as_ref() != Some(name));
        if !unassigned.is_empty() {
            self.construction.held += unassigned.heap();
            self.construction.sites.push(Site {
                class,
                kind: SiteKind::Escape,
                unassigned,
                span,
            });
        }
    }

    /// Checks `receiver` of a call of `member`. When it is `self` and the
    /// class has that method, the call is recorded rather than a use of
    /// `self` as a value.
    pub(super) fn member_receiver(&mut self, receiver: &'a Expr, member: &str) -> Ty {
        let callee = match (&receiver.node, self.frame.owner, self.frame.instance) {
            (Node::Var(name), Some(ns), true)
                if name.as_str() == "self" && member != "initialize" =>
            {
                self.program.namespaces[ns as usize]
                    .methods
                    .get(member)
                    .copied()
            }
            _ => None,
        };
        let Some(callee) = callee else {
            return self.expr(receiver, None);
        };
        let span = self.spans.expr(receiver);
        self.call_on_self(callee, span);
        self.self_receiver = true;
        let ty = self.expr(receiver, None);
        self.self_receiver = false;
        ty
    }

    /// Records which instance methods' results the checker proves: those
    /// of the classes whose instances no method can observe unassigned.
    pub(super) fn finish_construction(&mut self) {
        let mut unproven: HashSet<NsId> = (0..self.program.namespaces.len() as NsId)
            .filter(|&ns| !self.initializes(ns))
            .collect();
        self.transient(super::meter::set(&unproven));
        let reads = self.method_reads();
        for site in std::mem::take(&mut self.construction.sites) {
            if self.over_budget() {
                return;
            }
            self.meter.charge(site.unassigned.len() as u64);
            let observed: Vec<String> = match &site.kind {
                SiteKind::Read(name) => site
                    .unassigned
                    .iter()
                    .filter(|ivar| *ivar == name)
                    .cloned()
                    .collect(),
                SiteKind::Call(callee) => match reads.get(callee) {
                    Some(Some(read)) => site
                        .unassigned
                        .iter()
                        .filter(|ivar| read.contains(*ivar))
                        .cloned()
                        .collect(),
                    Some(None) => site.unassigned.clone(),
                    None => Vec::new(),
                },
                SiteKind::Escape => site.unassigned.clone(),
            };
            if !observed.is_empty() {
                unproven.insert(site.class);
                self.unassigned_read(&site, &observed);
            }
        }
        for decl in &self.program.fns {
            if let (Some(def), Some(owner), true) = (decl.def, decl.owner, decl.instance) {
                if !unproven.contains(&owner) {
                    self.facts.record_result(def);
                }
            }
        }
    }

    /// Reports a read of variables `ivars` of an instance being built
    /// before they are assigned, which reads `nil` whatever their types.
    fn unassigned_read(&mut self, site: &Site, ivars: &[String]) {
        let names = ivars
            .iter()
            .map(|ivar| format!("@{ivar}"))
            .collect::<Vec<_>>()
            .join(", ");
        let (they, them, are, reads) = if ivars.len() == 1 {
            ("it", "it", "is", "reads")
        } else {
            ("they", "them", "are", "read")
        };
        let message = match &site.kind {
            SiteKind::Read(_) => format!(
                "{names} {are} read before `initialize` assigns {them}, and {they} {reads} as nil; assign {them} first or give {them} a default in the class body"
            ),
            SiteKind::Call(callee) => {
                let method = self.program.fns[*callee]
                    .def
                    .map_or_else(String::new, |def| def.name.to_string());
                format!(
                    "`{method}` reads {names} before `initialize` assigns {them}, and {they} {reads} as nil; assign {them} before this call or give {them} a default in the class body"
                )
            }
            SiteKind::Escape => format!(
                "`self` is used before `initialize` assigns {names}, and {they} {reads} as nil; assign {them} first or give {them} a default in the class body"
            ),
        };
        self.report(Diagnostic::error(
            Code::UNINITIALIZED_IVAR,
            site.span,
            message,
        ));
    }

    /// Whether every instance of class `ns` has its required instance
    /// variables assigned once constructed: its `initialize` assigns them,
    /// as V0205 requires, or it has none.
    fn initializes(&mut self, ns: NsId) -> bool {
        let namespace = &self.program.namespaces[ns as usize];
        if namespace.module.is_none() || !namespace.is_class {
            return false;
        }
        if namespace.methods.contains_key("initialize") {
            return true;
        }
        let ivars: Vec<(bool, Ty)> = namespace
            .ivars
            .values()
            .map(|ivar| (ivar.default, ivar.ty))
            .collect();
        ivars
            .into_iter()
            .all(|(default, ty)| default || self.types.assignable(Ty::NIL, ty))
    }

    /// The instance variables each method reads, directly or through the
    /// methods it calls on `self`; `None` when `self` escapes, so it may
    /// read any.
    fn method_reads(&mut self) -> HashMap<FnId, Option<BTreeSet<String>>> {
        let mut reads: HashMap<FnId, Option<BTreeSet<String>>> = self
            .construction
            .methods
            .iter()
            .map(|(&id, uses)| (id, (!uses.escapes).then(|| uses.reads.clone())))
            .collect();
        self.construction.held += reads.heap();
        let mut changed = true;
        while changed {
            changed = false;
            for (id, uses) in &self.construction.methods {
                for callee in &uses.calls {
                    self.meter.charge(1);
                    if callee == id {
                        continue;
                    }
                    let callee_reads = reads.get(callee).cloned().unwrap_or(Some(BTreeSet::new()));
                    let caller = reads.get_mut(id).unwrap();
                    match (caller.as_mut(), callee_reads) {
                        (None, _) => {}
                        (Some(_), None) => {
                            *caller = None;
                            changed = true;
                        }
                        (Some(caller), Some(callee_reads)) => {
                            for ivar in callee_reads {
                                let bytes = btree_entry(caller) + ivar.len();
                                if caller.insert(ivar) {
                                    changed = true;
                                    self.construction.held += bytes;
                                }
                            }
                        }
                    }
                }
            }
        }
        reads
    }
}
