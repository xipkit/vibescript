//! What the checker knows about each local at one point of a function: its
//! current, possibly narrowed, type and whether every path assigned it.
//!
//! Branches mutate one state and record each change on a trail, so exploring
//! a branch and joining it with its siblings costs time proportional to the
//! changes the branch made, not to the number of locals.

use super::{
    counted::{CountedSet, CountedVec, Refused},
    marks::Marks,
    meter::{Meter, table as map_of},
    ty::{Ty, Types},
};
use std::{collections::HashMap, sync::Arc};

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

pub(crate) struct Flow {
    vars: CountedVec<VarState>,
    trail: CountedVec<(LocalId, VarState)>,
    /// Whether control can reach the current point.
    pub live: bool,
    /// The check's account, which the flow's work is charged to.
    meter: Arc<Meter>,
    /// The locals whose assignment [`Self::unassigned`] follows, from the
    /// first of them, with the set of those not assigned yet, which copies
    /// share rather than copy.
    tracked: Option<(LocalId, Marks)>,
}

impl super::meter::Heap for Branch {
    fn heap(&self) -> usize {
        super::meter::vec(&self.changes)
    }
}

impl Flow {
    pub fn new(meter: Arc<Meter>) -> Self {
        Self {
            vars: CountedVec::new(),
            trail: CountedVec::new(),
            live: true,
            meter,
            tracked: None,
        }
    }

    /// Follows the assignment of the `count` locals from `first`, none of
    /// which is assigned yet, once the set of them is counted; a set the
    /// budget refuses is not followed.
    pub fn track(&mut self, first: LocalId, count: usize) {
        if self.meter.tables().keep(Marks::most(count)).is_ok() {
            self.tracked = Some((first, Marks::all(count)));
        }
    }

    /// Stops following them.
    pub fn untrack(&mut self) {
        self.tracked = None;
    }

    /// Which of the followed locals are not assigned yet, by their place
    /// after the first.
    pub fn unassigned(&self) -> Option<&Marks> {
        self.tracked.as_ref().map(|(_, marks)| marks)
    }

    /// Notes that local `id` went from assigned or not to `assigned`.
    /// Returns the bytes of the nodes the followed locals' set copied.
    fn note(&mut self, id: LocalId, was: bool, assigned: bool) -> usize {
        if was != assigned {
            if let Some((first, marks)) = &mut self.tracked {
                if let Some(index) = id.checked_sub(*first) {
                    return marks.set(index as usize, !assigned);
                }
            }
        }
        0
    }

    /// The bytes [`Self::note`] would copy.
    fn cost(&self, id: LocalId, was: bool, assigned: bool) -> usize {
        match &self.tracked {
            Some((first, marks)) if was != assigned => id
                .checked_sub(*first)
                .map_or(0, |index| marks.cost(index as usize, !assigned)),
            _ => 0,
        }
    }

    /// Makes room for one more local, counted first.
    #[must_use = "a refusal stops the check, whose table must then keep nothing more"]
    pub fn reserve(&mut self) -> Result<(), Refused> {
        self.vars.reserve(self.meter.tables(), 1)
    }

    /// Adds a local, unassigned, in the room [`Self::reserve`] made.
    pub fn add(&mut self, declared: Ty) -> LocalId {
        self.vars.push_within(VarState {
            ty: declared,
            assigned: false,
        });
        (self.vars.len() - 1) as LocalId
    }

    /// The bytes the states and the trail hold.
    pub fn bytes(&self) -> usize {
        super::meter::vec(self.vars.as_vec())
            + super::meter::vec(self.trail.as_vec())
            + self.tracked.as_ref().map_or(0, |(_, marks)| marks.bytes())
    }

    pub fn get(&self, id: LocalId) -> VarState {
        self.vars[id as usize]
    }

    /// Changes local `id`'s state, recording the change to undo. The
    /// change and what following it copies are counted first, and a change
    /// the budget refuses is not made: the check has stopped, and reads no
    /// more.
    pub fn set(&mut self, id: LocalId, state: VarState) {
        let old = self.vars[id as usize];
        if old != state {
            let tables = self.meter.tables();
            let copies = self.cost(id, old.assigned, state.assigned);
            if self.trail.reserve(tables, 1).is_err() || tables.keep(copies).is_err() {
                return;
            }
            self.trail.push_within((id, old));
            self.vars[id as usize] = state;
            self.note(id, old.assigned, state.assigned);
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
        // A check past its budget unwinds without collecting the changes,
        // which nothing reads after it stops.
        if self.meter.stopped() {
            self.trail.truncate(mark.trail);
            self.live = mark.live;
            return Branch {
                live,
                changes: Vec::new(),
            };
        }
        // The changes are collected in tables that count their growth.
        let mut changes = CountedVec::new();
        let mut seen = CountedSet::new();
        let mut stopped = false;
        while self.trail.len() > mark.trail {
            let (id, old) = self.trail.pop().unwrap();
            let current = self.vars[id as usize];
            // A check this stops still undoes every change, but collects no
            // more of them.
            if !stopped {
                let tables = self.meter.tables();
                stopped = self.meter.charge(1)
                    || match seen.insert(tables, id) {
                        Ok(true) => changes.push(tables, (id, current)).is_err(),
                        Ok(false) => false,
                        Err(Refused) => true,
                    };
            }
            self.vars[id as usize] = old;
            let copied = self.note(id, current.assigned, old.assigned);
            self.meter.tables().kept(copied);
        }
        self.live = mark.live;
        Branch {
            live,
            changes: if stopped {
                Vec::new()
            } else {
                changes.into_vec()
            },
        }
    }

    /// The changes since `mark`, without undoing them: the state at an early
    /// exit such as `break`, which the enclosing loop joins.
    pub fn peek(&mut self, mark: Mark) -> Branch {
        if self.meter.stopped() {
            return Branch {
                live: true,
                changes: Vec::new(),
            };
        }
        let mut changes = CountedVec::new();
        let mut seen = CountedSet::new();
        let steps = (self.trail.len() - mark.trail) as u64;
        let tables = self.meter.tables();
        let mut stopped = self.meter.charge(steps);
        for &(id, _) in &self.trail[mark.trail..] {
            if stopped {
                break;
            }
            stopped = match seen.insert(tables, id) {
                Ok(true) => changes.push(tables, (id, self.vars[id as usize])).is_err(),
                Ok(false) => false,
                Err(Refused) => true,
            };
        }
        Branch {
            live: true,
            changes: if stopped {
                Vec::new()
            } else {
                changes.into_vec()
            },
        }
    }

    /// About what [`Self::join`] holds while it joins `branches`: a table of
    /// each one's changes, and a list of the locals they change.
    pub fn join_scratch(branches: &[Branch]) -> usize {
        branches
            .iter()
            .map(|branch| {
                let changes = branch.changes.len();
                map_of::<(LocalId, VarState)>(changes)
                    + changes * std::mem::size_of::<LocalId>()
                    + std::mem::size_of::<HashMap<LocalId, VarState>>()
            })
            .sum()
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
        let finals: Vec<HashMap<LocalId, VarState>> = live
            .iter()
            .map(|branch| branch.changes.iter().copied().collect())
            .collect();
        let mut ids: Vec<LocalId> = finals.iter().flat_map(|map| map.keys().copied()).collect();
        ids.sort_unstable();
        ids.dedup();
        for id in ids {
            // A union below can stop the check, which then reads no more.
            if self.meter.stopped() || self.meter.charge(live.len() as u64) {
                break;
            }
            let base = self.vars[id as usize];
            let states: Vec<VarState> = finals
                .iter()
                .map(|map| map.get(&id).copied().unwrap_or(base))
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
