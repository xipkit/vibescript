//! Proves that no method returns an instance variable its instance has not
//! assigned yet, so the runtime need not check instance method results.
//!
//! A required instance variable, one without a default whose type admits no
//! `nil`, reads as `nil` until `initialize` assigns it. While building an
//! instance, in `initialize` and in the instance variables' defaults, the
//! checker records each read of an unassigned variable, each call of one of
//! the class's methods on `self` and each other use of `self`. Once every
//! method is checked, a class is proven when none of these can read an
//! unassigned variable: no call reaches a method that reads one, directly
//! or through the methods it calls on `self`, and `self` is not used as a
//! value while one is unassigned, since the value can then reach any
//! method. A class with required variables and no `initialize` is not
//! proven. The checker does not report these programs, which the runtime
//! runs as it always has; the methods of a class it does not prove keep
//! their result check.

use super::{
    Checker,
    program::{FnId, NsId},
    ty::Ty,
};
use crate::syntax::{Expr, Node};
use std::collections::{BTreeSet, HashMap, HashSet};

/// What the checker records about instance variable reads.
#[derive(Default)]
pub(crate) struct Construction {
    /// What each instance method does with `self`.
    methods: HashMap<FnId, Uses>,
    /// Uses of `self` while it has unassigned variables.
    sites: Vec<Site>,
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
}

enum SiteKind {
    Read(String),
    Call(FnId),
    Escape,
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

    fn site(&mut self, kind: SiteKind) {
        let Some(class) = self.frame.owner else {
            return;
        };
        let unassigned = self.unassigned();
        if !unassigned.is_empty() {
            self.construction.sites.push(Site {
                class,
                kind,
                unassigned,
            });
        }
    }

    /// Records a read of instance variable `name` of `self`.
    pub(super) fn read_ivar(&mut self, name: &str) {
        if let Some(uses) = self.uses() {
            uses.reads.insert(name.to_owned());
        }
        self.site(SiteKind::Read(name.to_owned()));
    }

    /// Records a call of method `callee` on `self`.
    pub(super) fn call_on_self(&mut self, callee: FnId) {
        if let Some(uses) = self.uses() {
            uses.calls.insert(callee);
        }
        self.site(SiteKind::Call(callee));
    }

    /// Records a use of `self` as a value.
    pub(super) fn self_escapes(&mut self) {
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
        self.site(SiteKind::Escape);
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
        self.call_on_self(callee);
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
        let reads = self.method_reads();
        for site in std::mem::take(&mut self.construction.sites) {
            self.steps += site.unassigned.len() as u64;
            let observes = match &site.kind {
                SiteKind::Read(name) => site.unassigned.contains(name),
                SiteKind::Call(callee) => match reads.get(callee) {
                    Some(Some(read)) => site.unassigned.iter().any(|ivar| read.contains(ivar)),
                    Some(None) => true,
                    None => false,
                },
                SiteKind::Escape => true,
            };
            if observes {
                unproven.insert(site.class);
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
        let mut changed = true;
        while changed {
            changed = false;
            for (id, uses) in &self.construction.methods {
                for callee in &uses.calls {
                    self.steps += 1;
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
                                changed |= caller.insert(ivar);
                            }
                        }
                    }
                }
            }
        }
        reads
    }
}
