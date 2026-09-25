//! What the checker knows about each local at one point of a function: its
//! current, possibly narrowed, type and whether every path assigned it.
//!
//! Branches mutate one state and record each change on a trail, so exploring
//! a branch and joining it with its siblings costs time proportional to the
//! changes the branch made, not to the number of locals.

use super::ty::{Ty, Types};
use std::collections::HashMap;

pub(crate) type LocalId = u32;

/// A local's state on the current path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct VarState {
    /// The narrowed type, never wider than the declared one.
    pub ty: Ty,
    pub assigned: bool,
}

/// A point to return to after exploring a branch.
#[derive(Clone, Copy)]
pub(crate) struct Mark {
    trail: usize,
    live: bool,
}

/// The end of one explored branch: whether control reached its end, and the
/// final state of each local it changed.
pub(crate) struct Branch {
    pub live: bool,
    pub changes: Vec<(LocalId, VarState)>,
}

#[derive(Default)]
pub(crate) struct Flow {
    pub vars: Vec<VarState>,
    trail: Vec<(LocalId, VarState)>,
    /// Whether control can reach the current point.
    pub live: bool,
}

impl Flow {
    pub fn new() -> Self {
        Self {
            vars: Vec::new(),
            trail: Vec::new(),
            live: true,
        }
    }

    /// Adds a local, unassigned.
    pub fn add(&mut self, declared: Ty) -> LocalId {
        self.vars.push(VarState {
            ty: declared,
            assigned: false,
        });
        (self.vars.len() - 1) as LocalId
    }

    pub fn get(&self, id: LocalId) -> VarState {
        self.vars[id as usize]
    }

    pub fn set(&mut self, id: LocalId, state: VarState) {
        let old = self.vars[id as usize];
        if old != state {
            self.trail.push((id, old));
            self.vars[id as usize] = state;
        }
    }

    pub fn mark(&self) -> Mark {
        Mark {
            trail: self.trail.len(),
            live: self.live,
        }
    }

    /// Undoes every change since `mark`, returning the branch that made them.
    pub fn rollback(&mut self, mark: Mark) -> Branch {
        let live = self.live;
        let mut changes: Vec<(LocalId, VarState)> = Vec::new();
        let mut seen: HashMap<LocalId, ()> = HashMap::new();
        while self.trail.len() > mark.trail {
            let (id, old) = self.trail.pop().unwrap();
            if seen.insert(id, ()).is_none() {
                changes.push((id, self.vars[id as usize]));
            }
            self.vars[id as usize] = old;
        }
        self.live = mark.live;
        Branch { live, changes }
    }

    /// The changes since `mark`, without undoing them: the state at an early
    /// exit such as `break`, which the enclosing loop joins.
    pub fn peek(&self, mark: Mark) -> Branch {
        let mut changes: Vec<(LocalId, VarState)> = Vec::new();
        for &(id, _) in &self.trail[mark.trail..] {
            if !changes.iter().any(|(seen, _)| *seen == id) {
                changes.push((id, self.vars[id as usize]));
            }
        }
        Branch {
            live: true,
            changes,
        }
    }

    /// Continues after sibling branches: a local's type is the union of its
    /// types on the branches that reach the end, and it is assigned only if
    /// each of them assigned it. `declared` gives a local's declared type.
    pub fn join(
        &mut self,
        types: &mut Types,
        branches: Vec<Branch>,
        declared: &dyn Fn(LocalId) -> Ty,
    ) {
        let live: Vec<&Branch> = branches.iter().filter(|b| b.live).collect();
        if live.is_empty() {
            self.live = false;
            return;
        }
        let mut ids: Vec<LocalId> = live
            .iter()
            .flat_map(|branch| branch.changes.iter().map(|(id, _)| *id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            let base = self.vars[id as usize];
            let states: Vec<VarState> = live
                .iter()
                .map(|branch| {
                    branch
                        .changes
                        .iter()
                        .find(|(changed, _)| *changed == id)
                        .map_or(base, |(_, state)| *state)
                })
                .collect();
            let assigned = states.iter().all(|state| state.assigned);
            let tys: Vec<Ty> = states
                .iter()
                .filter(|state| state.assigned)
                .map(|state| state.ty)
                .collect();
            let ty = if tys.is_empty() {
                declared(id)
            } else {
                types.union(&tys)
            };
            self.set(id, VarState { ty, assigned });
        }
        self.live = true;
    }
}
