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
    counted::{CountedBTreeSet, CountedMap, CountedSet, CountedVec},
    marks::Marks,
    meter::{Heap, btree_storage},
    program::{FnId, NsId},
    ty::Ty,
};
use crate::{
    diagnostic::{Code, Diagnostic, Span},
    syntax::{Expr, Node},
};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    rc::Rc,
};

/// A class's required instance variables, in name order, which the sets of
/// those not assigned yet index.
pub(crate) type Roster = Rc<[String]>;

/// What a roster holds.
pub(crate) fn roster_bytes(roster: &Roster) -> usize {
    std::mem::size_of_val(&**roster) + roster.iter().map(String::capacity).sum::<usize>()
}

/// The variables of an instance being built that are not assigned yet at
/// some point: places in their roster, shared with the points before and
/// after it rather than copied.
#[derive(Clone)]
pub(crate) struct Unassigned {
    roster: Roster,
    marks: Marks,
}

impl Unassigned {
    /// Every variable of `roster`.
    pub fn all(roster: Roster) -> Self {
        let marks = Marks::all(roster.len());
        Self { roster, marks }
    }

    pub fn len(&self) -> usize {
        self.marks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.marks.is_empty()
    }

    /// The place of variable `name` in the roster.
    fn place(&self, name: &str) -> Option<usize> {
        self.roster
            .binary_search_by(|ivar| ivar.as_str().cmp(name))
            .ok()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.place(name)
            .is_some_and(|place| self.marks.contains(place))
    }

    /// The bytes [`Self::remove`] of `name` would copy.
    pub fn cost(&self, name: &str) -> usize {
        self.place(name)
            .map_or(0, |place| self.marks.cost(place, false))
    }

    /// Takes out variable `name`; returns the bytes this copied.
    pub fn remove(&mut self, name: &str) -> usize {
        match self.place(name) {
            Some(place) => self.marks.set(place, false),
            None => 0,
        }
    }

    /// The variables, in name order.
    fn names(&self) -> Vec<&str> {
        self.marks
            .indices()
            .into_iter()
            .map(|place| self.roster[place].as_str())
            .collect()
    }

    /// Those of the variables `read` names, in name order, going through
    /// whichever of the two is shorter; returns them with that length,
    /// the work it took.
    fn read<'s>(&'s self, read: &'s BTreeSet<String>) -> (Vec<&'s str>, usize) {
        if read.len() < self.len() {
            let names = read
                .iter()
                .filter(|name| self.contains(name))
                .map(String::as_str)
                .collect();
            (names, read.len())
        } else {
            let names = self
                .marks
                .indices()
                .into_iter()
                .map(|place| self.roster[place].as_str())
                .filter(|name| read.contains(*name))
                .collect();
            (names, self.len())
        }
    }

    /// The most bytes [`Self::read`] of `read`, or [`Self::names`] when
    /// `read` is `None`, lists.
    fn listing(&self, read: Option<&BTreeSet<String>>) -> usize {
        let entry = std::mem::size_of::<&str>();
        match read {
            Some(read) if read.len() < self.len() => read.len() * entry,
            _ => self.len() * (std::mem::size_of::<usize>() + entry),
        }
    }
}

/// What the checker records about instance variable reads.
#[derive(Default)]
pub(crate) struct Construction {
    /// What each instance method does with `self`.
    methods: CountedMap<FnId, Uses>,
    /// Uses of `self` while it has unassigned variables.
    sites: CountedVec<Site>,
    /// What the methods' uses and the sites hold beyond the tables' own
    /// storage, for the checker's memory account.
    held: usize,
    /// The addresses of the rosters and of the nodes of the sets of
    /// unassigned variables that the sites keep, which they share, so each
    /// is counted in `held` once. The sites keep them after the frames
    /// that made them are gone.
    retained: CountedSet<usize>,
}

impl Construction {
    /// What the records hold.
    pub fn bytes(&self) -> usize {
        super::meter::map(&self.methods)
            + super::meter::vec(self.sites.as_vec())
            + super::meter::set(&self.retained)
            + self.held
    }
}

/// What a method does with `self`, directly or in its blocks.
#[derive(Default)]
struct Uses {
    reads: CountedBTreeSet<String>,
    calls: CountedBTreeSet<FnId>,
    /// Whether `self` is used as a value, which lets any method read it.
    escapes: bool,
}

/// A use of `self` while it has unassigned variables.
struct Site {
    class: NsId,
    kind: SiteKind,
    unassigned: Unassigned,
    span: Span,
}

/// The name a read keeps; the roster and the set of variables it shares
/// are counted as the sites first keep them.
impl super::counted::Owned for Site {
    fn owned(&self) -> usize {
        self.kind.heap()
    }
}

/// Its sets are counted as they grow.
impl super::counted::Owned for Uses {
    fn owned(&self) -> usize {
        0
    }
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
    /// yet where the code being checked runs, if it builds one and some
    /// are. They are shared with the other points that ask, not copied.
    fn unassigned(&self) -> Option<Unassigned> {
        if !self.frame.flow.live {
            return None;
        }
        let unassigned = match &self.frame.building {
            Some(building) => building.clone(),
            None => {
                let (roster, _) = self.frame.initialize.as_ref()?;
                Unassigned {
                    roster: Rc::clone(roster),
                    marks: self.frame.flow.unassigned()?.clone(),
                }
            }
        };
        (!unassigned.is_empty()).then_some(unassigned)
    }

    fn site(&mut self, kind: SiteKind, span: Span) {
        let Some(class) = self.frame.owner else {
            return;
        };
        if let Some(unassigned) = self.unassigned() {
            self.record_site(Site {
                class,
                kind,
                unassigned,
                span,
            });
        }
    }

    /// Keeps `site`, counting what it holds that no earlier site shares:
    /// its roster, and the nodes of its set of unassigned variables that
    /// the assignments before it copied. The addresses it records, and
    /// room for the site, are counted before they are kept, and a check
    /// the budget stops keeps no more sites.
    fn record_site(&mut self, site: Site) {
        let tables = self.meter.tables();
        let construction = &mut self.construction;
        let roster = Rc::as_ptr(&site.unassigned.roster).cast::<u8>() as usize;
        match construction.retained.insert(tables, roster) {
            Ok(true) => construction.held += roster_bytes(&site.unassigned.roster),
            Ok(false) => (),
            Err(_) => return,
        }
        let Some((bytes, visited)) = site
            .unassigned
            .marks
            .retain(&mut construction.retained, tables)
        else {
            return;
        };
        construction.held += bytes + site.kind.heap();
        // A check that finding what it shares stops keeps no more sites,
        // and a site the budget refuses room for is not kept.
        if self.meter.charge(visited as u64) || self.construction.sites.push(tables, site).is_err()
        {
            self.stopped = true;
        }
    }

    /// Records a read of instance variable `name` of `self` at `span`.
    pub(super) fn read_ivar(&mut self, name: &str, span: Span) {
        let tables = self.meter.tables();
        if let Some(uses) = uses(&mut self.construction, self.frame.function, tables) {
            // The name, and the set's room for it, are counted before it
            // is kept; a check the budget stops records no more.
            if !uses.reads.contains(name) {
                let Ok(mut kept) = tables.keep(name.len()) else {
                    return;
                };
                let before = btree_storage::<String>(uses.reads.len());
                if uses
                    .reads
                    .insert_kept(tables, &mut kept, name.to_owned())
                    .is_err()
                {
                    return;
                }
                let bytes = btree_storage::<String>(uses.reads.len()) - before + name.len();
                self.construction.held += bytes;
            }
        }
        self.site(SiteKind::Read(self.copy(name)), span);
    }

    /// Records a call of method `callee` on `self` at `span`.
    pub(super) fn call_on_self(&mut self, callee: FnId, span: Span) {
        let tables = self.meter.tables();
        if let Some(uses) = uses(&mut self.construction, self.frame.function, tables) {
            let before = btree_storage::<FnId>(uses.calls.len());
            // A check the budget stops records no more.
            if uses.calls.insert(tables, callee).is_err() {
                return;
            }
            let bytes = btree_storage::<FnId>(uses.calls.len()) - before;
            self.construction.held += bytes;
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
        let tables = self.meter.tables();
        if let Some(uses) = uses(&mut self.construction, self.frame.function, tables) {
            uses.escapes = true;
        }
        let Some(class) = self.frame.owner else {
            return;
        };
        let Some(mut unassigned) = self.unassigned() else {
            return;
        };
        // The path the removal copies is held before it is made, until
        // the site, which counts what it keeps, takes it.
        let copied = stored.as_ref().map_or(0, |stored| unassigned.cost(stored));
        let Some(held) = self.hold(copied) else {
            return;
        };
        if let Some(stored) = &stored {
            unassigned.remove(stored);
        }
        if !unassigned.is_empty() {
            self.record_site(Site {
                class,
                kind: SiteKind::Escape,
                unassigned,
                span,
            });
        }
        self.release(held);
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
        // The classes not proven, at most every namespace: held at that
        // size, at which the set is made, so that the classes the sites
        // add later fit without growing it, while the analysis runs.
        let namespaces = self.program.namespaces.len();
        let Some(set_held) = self.hold(super::meter::table::<NsId>(namespaces)) else {
            return;
        };
        let mut unproven: HashSet<NsId> = HashSet::with_capacity(namespaces);
        unproven.extend((0..namespaces as NsId).filter(|&ns| !self.initializes(ns)));
        let (reads, reads_held) = self.method_reads();
        let sites = std::mem::take(&mut self.construction.sites);
        // Taken from the records, the sites are held while they are read,
        // as the methods' reads are.
        let Some(sites_held) = self.hold(super::meter::vec(sites.as_vec())) else {
            self.release(reads_held + set_held);
            return;
        };
        let taken = reads_held + sites_held + set_held;
        for site in sites {
            if self.over_budget() {
                self.release(taken);
                return;
            }
            // Each site costs what it looks at: its read variable, the
            // shorter of its callee's reads and its unassigned variables,
            // or all of those for `self` used as a value.
            // The variables a call or `self` observes: those its callee
            // reads, or all of them; the list of them is counted before it
            // is made.
            let listed = match &site.kind {
                SiteKind::Read(_) => None,
                SiteKind::Call(callee) => reads.get(callee).map(|read| (**read).as_ref()),
                SiteKind::Escape => Some(None),
            };
            if self.transient(listed.map_or(0, |read| site.unassigned.listing(read))) {
                self.release(taken);
                return;
            }
            let (observed, work) = match (&site.kind, listed) {
                (SiteKind::Read(name), _) => {
                    let found = site.unassigned.contains(name);
                    (
                        if found {
                            vec![name.as_str()]
                        } else {
                            Vec::new()
                        },
                        1,
                    )
                }
                (_, Some(Some(read))) => site.unassigned.read(read),
                (_, Some(None)) => (site.unassigned.names(), site.unassigned.len()),
                (_, None) => (Vec::new(), 1),
            };
            if self.meter.charge(work as u64) {
                self.release(taken);
                return;
            }
            if !observed.is_empty() {
                unproven.insert(site.class);
                self.unassigned_read(&site, &observed);
            }
        }
        for decl in &self.program.fns {
            if let (Some(def), Some(owner), true) = (decl.def, decl.owner, decl.instance) {
                if !unproven.contains(&owner) && !self.facts.record_result(self.meter.tables(), def)
                {
                    break;
                }
            }
        }
        self.release(taken);
    }

    /// Reports a read of variables `ivars` of an instance being built
    /// before they are assigned, which reads `nil` whatever their types.
    fn unassigned_read(&mut self, site: &Site, ivars: &[&str]) {
        let (names, _) = super::listed(&self.meter, ivars, |out, ivar| {
            out.push('@');
            out.push_str(ivar);
        });
        if self.halted() {
            return;
        }
        let (they, them, are, reads) = if ivars.len() == 1 {
            ("it", "it", "is", "reads")
        } else {
            ("they", "them", "are", "read")
        };
        let message = match &site.kind {
            SiteKind::Read(_) => text!(
                self,
                "{names} {are} read before `initialize` assigns {them}, and {they} {reads} as nil; assign {them} first or give {them} a default in the class body"
            ),
            SiteKind::Call(callee) => {
                let method = self.program.fns[*callee]
                    .def
                    .map_or_else(String::new, |def| self.copy(&def.name));
                text!(
                    self,
                    "`{method}` reads {names} before `initialize` assigns {them}, and {they} {reads} as nil; assign {them} before this call or give {them} a default in the class body"
                )
            }
            SiteKind::Escape => text!(
                self,
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
        // Read in place rather than copied.
        let types = &mut self.types;
        namespace
            .ivars
            .values()
            .all(|ivar| ivar.default || types.assignable(Ty::NIL, ivar.ty))
    }

    /// The instance variables each method reads, directly or through the
    /// methods it calls on `self`; `None` when `self` escapes, so it may
    /// read any.
    ///
    /// Methods that call each other in a cycle read the same variables, so
    /// each cycle is found once, and each is done after those it calls,
    /// adding what they read once per call rather than again on every pass
    /// until nothing changes. Each call, and each variable a method or a
    /// call adds, is a step, as is each method and call the search for
    /// cycles visits. Returns the reads with the scratch that stays held
    /// for them, for the caller to release; a check the budget stops finds
    /// none.
    fn method_reads(&mut self) -> (Reads, usize) {
        // The methods, their places and the calls between them, counted
        // before they are listed: at most every call each method makes.
        let methods = self.construction.methods.len();
        let calls: usize = self
            .construction
            .methods
            .values()
            .map(|method| method.calls.len())
            .sum();
        if self.transient(
            methods * std::mem::size_of::<FnId>()
                + super::meter::table::<(FnId, usize)>(methods)
                + methods * std::mem::size_of::<Vec<usize>>()
                + calls * std::mem::size_of::<usize>(),
        ) {
            return (HashMap::new(), 0);
        }
        let mut ids: Vec<FnId> = self.construction.methods.keys().copied().collect();
        if super::counted::sort_unstable_by(&self.meter, &mut ids, Ord::cmp).is_err() {
            return (HashMap::new(), 0);
        }
        let place: HashMap<FnId, usize> =
            ids.iter().enumerate().map(|(at, &id)| (id, at)).collect();
        let calls: Vec<Vec<usize>> = ids
            .iter()
            .map(|id| {
                self.construction.methods[id]
                    .calls
                    .iter()
                    .filter_map(|callee| place.get(callee).copied())
                    .collect()
            })
            .collect();
        // The graph is held while its cycles are found and their reads
        // gathered, with what that builds, counted before it is built: the
        // search's scratch, and at most a cycle for each method, each with
        // its list of methods and its shared reads, and the reads by method.
        let graph = super::meter::map(&place)
            + super::meter::vec(&calls)
            + calls.iter().map(super::meter::vec).sum::<usize>();
        // A cycle's reads themselves are counted as they are kept.
        let reads = std::mem::size_of::<Rc<Option<BTreeSet<String>>>>() + RC_COUNTS;
        let Some(held) = self.hold(
            graph
                + methods * (SEARCH_SCRATCH + CYCLE + reads)
                + super::meter::table::<(FnId, Rc<Option<BTreeSet<String>>>)>(methods),
        ) else {
            return (HashMap::new(), 0);
        };
        let Some((cycles, cycle_of)) = cycles(&calls, &self.meter) else {
            self.release(held);
            return (HashMap::new(), 0);
        };
        let mut found: Vec<Rc<Option<BTreeSet<String>>>> = Vec::with_capacity(cycles.len());
        for (cycle, members) in cycles.iter().enumerate() {
            // The reads it gathers are counted as each cycle's are kept.
            if self.over_budget() {
                self.release(held);
                return (HashMap::new(), 0);
            }
            let mut escapes = false;
            let mut read = CountedBTreeSet::new();
            for &member in members {
                let uses = &self.construction.methods[&ids[member]];
                if self
                    .meter
                    .charge(uses.calls.len() as u64 + uses.reads.len() as u64)
                {
                    self.release(held);
                    return (HashMap::new(), 0);
                }
                escapes |= uses.escapes;
                let tables = self.meter.tables();
                if !escapes && gather(&mut read, uses.reads.iter(), tables).is_err() {
                    self.release(held);
                    return (HashMap::new(), 0);
                }
                for &callee in &calls[member] {
                    let other = cycle_of[callee];
                    if other == cycle {
                        continue;
                    }
                    match &*found[other] {
                        None => escapes = true,
                        Some(called) => {
                            if self.meter.charge(called.len() as u64)
                                || (!escapes && gather(&mut read, called.iter(), tables).is_err())
                            {
                                self.release(held);
                                return (HashMap::new(), 0);
                            }
                        }
                    }
                }
            }
            let read = (!escapes).then(|| read.into_set());
            self.construction.held += read.heap() + std::mem::size_of::<Option<BTreeSet<String>>>();
            found.push(Rc::new(read));
        }
        let reads: Reads = ids
            .iter()
            .enumerate()
            .map(|(at, &id)| (id, Rc::clone(&found[cycle_of[at]])))
            .collect();
        // The graph, the search's scratch and the cycles go here; only the
        // reads by method stay, held while the sites are read.
        drop((place, calls, cycles, cycle_of, found));
        self.release(held);
        let Some(kept) = self.hold(super::meter::map(&reads)) else {
            return (HashMap::new(), 0);
        };
        (reads, kept)
    }
}

/// Adds `names` to the reads `read` gathers, each new one's copy counted
/// to `tables`, with its room in the set, before it is kept.
fn gather<'n>(
    read: &mut CountedBTreeSet<String>,
    names: impl Iterator<Item = &'n String>,
    tables: super::counted::Ledger<'_>,
) -> Result<(), super::counted::Refused> {
    for name in names {
        if !read.contains(name) {
            let mut kept = tables.keep(name.len())?;
            read.insert_kept(tables, &mut kept, name.clone())?;
        }
    }
    Ok(())
}

/// What `function`, the function being checked, does with `self`, with
/// its entry counted to `tables` before it is made; `None` outside one, or
/// when the budget refuses it, which stops the check.
fn uses<'c>(
    construction: &'c mut Construction,
    function: Option<FnId>,
    tables: super::counted::Ledger<'_>,
) -> Option<&'c mut Uses> {
    construction
        .methods
        .get_or_insert_with(tables, function?, Uses::default)
        .ok()
}

/// What each instance method reads of `self`, shared by the methods of a
/// cycle; `None` for those that let it escape.
type Reads = HashMap<FnId, Rc<Option<BTreeSet<String>>>>;

/// What finding the cycles of a graph holds for each of its nodes at most:
/// the search's order, low link, stack flag and cycle, its stack of nodes
/// and of frames, each of which can double as it grows.
const SEARCH_SCRATCH: usize = 3 * std::mem::size_of::<usize>()
    + std::mem::size_of::<bool>()
    + 2 * std::mem::size_of::<usize>()
    + 2 * std::mem::size_of::<(usize, usize)>();

/// What a cycle of one node holds at most: its place in the list of
/// cycles, which can double as it grows, and its list of nodes, which
/// starts with room for four.
const CYCLE: usize = 2 * std::mem::size_of::<Vec<usize>>() + 4 * std::mem::size_of::<usize>();

/// The counts beside an [`Rc`]'s value.
const RC_COUNTS: usize = 2 * std::mem::size_of::<usize>();

/// The cycles of the directed graph whose node `n` leads to each node in
/// `edges[n]`, each a list of its nodes, in an order where every cycle
/// comes after those its nodes lead to; with the cycle of each node.
/// Tarjan's algorithm, keeping its place on the heap rather than the stack.
/// Each node and edge it visits is a step of `meter`, which it asks every
/// [`PACE`](super::walk::PACE) of them whether the check has stopped, and
/// gives up, with `None`, if it has.
fn cycles(
    edges: &[Vec<usize>],
    meter: &super::meter::Meter,
) -> Option<(Vec<Vec<usize>>, Vec<usize>)> {
    use super::walk::PACE;
    const UNSEEN: usize = usize::MAX;
    let count = edges.len();
    let mut order = vec![UNSEEN; count];
    let mut low = vec![0; count];
    let mut on_stack = vec![false; count];
    let mut stack = Vec::new();
    let mut cycles: Vec<Vec<usize>> = Vec::new();
    let mut cycle_of = vec![UNSEEN; count];
    let mut next = 0;
    let mut visited = 0;
    let pace = |visited: &mut u64| {
        *visited += 1;
        if *visited == PACE {
            *visited = 0;
            meter.pace(PACE, 0)
        } else {
            false
        }
    };
    for root in 0..count {
        if pace(&mut visited) {
            return None;
        }
        if order[root] != UNSEEN {
            continue;
        }
        // Each frame is a node and how many of its edges it has followed.
        let mut frames = vec![(root, 0)];
        order[root] = next;
        low[root] = next;
        next += 1;
        stack.push(root);
        on_stack[root] = true;
        while let Some(&mut (node, ref mut followed)) = frames.last_mut() {
            if let Some(&to) = edges[node].get(*followed) {
                *followed += 1;
                if pace(&mut visited) {
                    return None;
                }
                if order[to] == UNSEEN {
                    order[to] = next;
                    low[to] = next;
                    next += 1;
                    stack.push(to);
                    on_stack[to] = true;
                    frames.push((to, 0));
                } else if on_stack[to] {
                    low[node] = low[node].min(order[to]);
                }
                continue;
            }
            frames.pop();
            if let Some(&(parent, _)) = frames.last() {
                low[parent] = low[parent].min(low[node]);
            }
            if low[node] == order[node] {
                let mut members = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack[member] = false;
                    cycle_of[member] = cycles.len();
                    members.push(member);
                    if member == node {
                        break;
                    }
                }
                cycles.push(members);
            }
        }
    }
    if meter.charge(visited) {
        return None;
    }
    Some((cycles, cycle_of))
}
