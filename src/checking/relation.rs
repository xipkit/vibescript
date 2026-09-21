use super::facts::{Atom, Fact, Facts, Node, same_bytes};
use crate::{CallContext, Result, budget::Buffer};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Relation {
    Accepted,
    Gradual,
    Rejected,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pair {
    source: Fact,
    target: Fact,
    keys: bool,
    overlap: bool,
}

impl Pair {
    fn bucket(self, mask: usize) -> usize {
        self.source
            .0
            .wrapping_mul(0x9e3779b1)
            .wrapping_add(self.target.0.wrapping_mul(0x85ebca77))
            .wrapping_add(usize::from(self.keys))
            .wrapping_add(usize::from(self.overlap) * 3)
            & mask
    }
}

struct MemoEntry {
    pair: Pair,
    relation: Relation,
    next: usize,
}

struct Memo {
    entries: Buffer<MemoEntry>,
    buckets: Buffer<usize>,
}

impl Memo {
    fn new() -> Self {
        Self {
            entries: Buffer::empty(),
            buckets: Buffer::empty(),
        }
    }

    fn get(&self, ctx: &mut CallContext, pair: Pair) -> Result<Option<Relation>> {
        ctx.charge(1)?;
        if self.buckets.data.is_empty() {
            return Ok(None);
        }
        let mut index = self.buckets.data[pair.bucket(self.buckets.data.len() - 1)];
        while index != usize::MAX {
            ctx.charge(1)?;
            let entry = &self.entries.data[index];
            if entry.pair == pair {
                return Ok(Some(entry.relation));
            }
            index = entry.next;
        }
        Ok(None)
    }

    fn insert(&mut self, ctx: &mut CallContext, pair: Pair, relation: Relation) -> Result<()> {
        if self.entries.data.len() >= self.buckets.data.len() / 2 {
            let Some(capacity) = self.buckets.data.len().max(8).checked_mul(2) else {
                return ctx.fail(
                    crate::ErrorKind::Memory,
                    "checker relation table size overflow",
                );
            };
            let mut buckets = Buffer::with_capacity(ctx, capacity)?;
            ctx.charge(capacity as u64)?;
            buckets.data.resize(capacity, usize::MAX);
            ctx.charge(self.entries.data.len() as u64)?;
            for (index, entry) in self.entries.data.iter_mut().enumerate() {
                let bucket = entry.pair.bucket(capacity - 1);
                entry.next = buckets.data[bucket];
                buckets.data[bucket] = index;
            }
            self.buckets = buckets;
        }
        let bucket = pair.bucket(self.buckets.data.len() - 1);
        let index = self.entries.data.len();
        self.entries.push(
            ctx,
            MemoEntry {
                pair,
                relation,
                next: self.buckets.data[bucket],
            },
        )?;
        self.buckets.data[bucket] = index;
        Ok(())
    }
}

enum Task {
    Visit(Pair),
    All(usize),
    Any(usize, Option<Pair>),
    CoverageCheck(Pair),
    CoverageNext(Pair, Buffer<Fact>, usize, Relation),
    Save(Pair),
}

impl Facts {
    pub fn relation(
        &mut self,
        ctx: &mut CallContext,
        source: Fact,
        target: Fact,
    ) -> Result<Relation> {
        self.compare(ctx, source, target, false)
    }

    /// Reports hash key contracts that runtime normalization rejects before it
    /// inspects any stored key, so even an empty hash fails them.
    ///
    /// Builtin key annotations other than `string`, `symbol` and `any`, such as
    /// `hash<int, int>`, answer this way. Nominal keys instead validate each stored
    /// key, which admits the empty hash. A never key fact is not a contract; it
    /// describes a hash known to be empty and keeps its ordinary comparison.
    pub(super) fn impossible_keys(&self, key: Fact) -> bool {
        key != Atom::Never.fact() && self.string_key(key) == Some(false)
    }

    pub fn overlaps(&mut self, ctx: &mut CallContext, source: Fact, target: Fact) -> Result<bool> {
        ctx.charge(1)?;
        Ok(source != Atom::Never.fact()
            && self.compare(ctx, source, target, true)? != Relation::Rejected)
    }

    fn compare(
        &mut self,
        ctx: &mut CallContext,
        source: Fact,
        target: Fact,
        overlap: bool,
    ) -> Result<Relation> {
        let mut tasks = Buffer::empty();
        let mut values = Buffer::empty();
        let mut memo = Memo::new();
        tasks.push(
            ctx,
            Task::Visit(Pair {
                source,
                target,
                keys: false,
                overlap,
            }),
        )?;
        while let Some(task) = tasks.data.pop() {
            ctx.charge(1)?;
            let value = match task {
                Task::Save(pair) => {
                    memo.insert(ctx, pair, *values.data.last().unwrap())?;
                    continue;
                }
                Task::CoverageCheck(pair) => {
                    let overlap = values.data.pop().unwrap();
                    if overlap != Relation::Rejected {
                        if let Some(variants) = self.split(ctx, pair.source)? {
                            let source = variants.data[0];
                            tasks.push(
                                ctx,
                                Task::CoverageNext(pair, variants, 1, Relation::Accepted),
                            )?;
                            tasks.push(ctx, Task::Visit(Pair { source, ..pair }))?;
                            continue;
                        }
                    }
                    Relation::Rejected
                }
                Task::CoverageNext(pair, variants, index, previous) => {
                    let result = all(previous, values.data.pop().unwrap());
                    if result != Relation::Rejected && index < variants.data.len() {
                        let source = variants.data[index];
                        tasks.push(ctx, Task::CoverageNext(pair, variants, index + 1, result))?;
                        tasks.push(ctx, Task::Visit(Pair { source, ..pair }))?;
                        continue;
                    }
                    result
                }
                Task::All(count) => {
                    let start = values.data.len() - count;
                    let mut result = Relation::Accepted;
                    for &value in &values.data[start..] {
                        ctx.charge(1)?;
                        result = all(result, value);
                    }
                    values.data.truncate(start);
                    result
                }
                Task::Any(count, coverage) => {
                    let start = values.data.len() - count;
                    let mut result = Relation::Rejected;
                    for &value in &values.data[start..] {
                        ctx.charge(1)?;
                        result = any(result, value);
                    }
                    values.data.truncate(start);
                    if result == Relation::Rejected {
                        if let Some(pair) = coverage {
                            tasks.push(ctx, Task::CoverageCheck(pair))?;
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    overlap: true,
                                    ..pair
                                }),
                            )?;
                            continue;
                        }
                    }
                    result
                }
                Task::Visit(pair) => {
                    if let Some(value) = memo.get(ctx, pair)? {
                        values.push(ctx, value)?;
                        continue;
                    }
                    tasks.push(ctx, Task::Save(pair))?;
                    if pair.keys {
                        if self.string_key(pair.target).is_none() {
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    keys: false,
                                    ..pair
                                }),
                            )?;
                            continue;
                        }
                        if let (Some(source), Some(target)) =
                            (self.string_key(pair.source), self.string_key(pair.target))
                        {
                            if source || target {
                                values.push(
                                    ctx,
                                    if source == target {
                                        Relation::Accepted
                                    } else {
                                        Relation::Rejected
                                    },
                                )?;
                                continue;
                            }
                        }
                    }
                    let source = self.node(pair.source);
                    let target = self.node(pair.target);
                    match (source, target) {
                        (_, Node::Atom(Atom::Any)) | (Node::Atom(Atom::Never), _) => {
                            Relation::Accepted
                        }
                        (Node::Atom(Atom::Unknown | Atom::Any), _)
                        | (_, Node::Atom(Atom::Unknown))
                        | (Node::Named(_), _)
                        | (_, Node::Named(_)) => Relation::Gradual,
                        _ if pair.source == pair.target => Relation::Accepted,
                        (Node::Instance { class, .. }, Node::Nominal { .. }) => {
                            if self.same_nominal(ctx, *class, pair.target)? {
                                Relation::Accepted
                            } else {
                                Relation::Rejected
                            }
                        }
                        (Node::Nominal { .. }, Node::Instance { class, .. }) => {
                            if self.same_nominal(ctx, pair.source, *class)? {
                                Relation::Gradual
                            } else {
                                Relation::Rejected
                            }
                        }
                        (Node::Nominal { .. }, Node::Nominal { .. }) => {
                            if self.same_nominal(ctx, pair.source, pair.target)? {
                                Relation::Accepted
                            } else {
                                Relation::Rejected
                            }
                        }
                        (
                            Node::EnumMember {
                                enumeration: a,
                                index: ai,
                            },
                            Node::EnumMember {
                                enumeration: b,
                                index: bi,
                            },
                        ) => {
                            if a != b || ai.is_some() && bi.is_some() {
                                Relation::Rejected
                            } else if bi.is_none() {
                                Relation::Accepted
                            } else {
                                Relation::Gradual
                            }
                        }
                        (Node::EnumMember { .. }, Node::Nominal { .. }) => {
                            if self.enum_nominal(pair.source) == Some(pair.target) {
                                Relation::Accepted
                            } else {
                                Relation::Rejected
                            }
                        }
                        (Node::Nominal { .. }, Node::EnumMember { index, .. }) => {
                            if self.enum_nominal(pair.target) == Some(pair.source) {
                                if index.is_none() {
                                    Relation::Accepted
                                } else {
                                    Relation::Gradual
                                }
                            } else {
                                Relation::Rejected
                            }
                        }
                        (Node::Protected(source, ..), _) => {
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    source: *source,
                                    ..pair
                                }),
                            )?;
                            continue;
                        }
                        (_, Node::Protected(target, ..)) => {
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    target: *target,
                                    ..pair
                                }),
                            )?;
                            continue;
                        }
                        (Node::Offset(source), Node::Offset(target)) => {
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    source: *source,
                                    target: *target,
                                    ..pair
                                }),
                            )?;
                            continue;
                        }
                        (Node::Union(arms) | Node::Choice(arms), _) => {
                            tasks.push(
                                ctx,
                                if pair.overlap {
                                    Task::Any(arms.data.len(), None)
                                } else {
                                    Task::All(arms.data.len())
                                },
                            )?;
                            for &source in arms.data.iter().rev() {
                                tasks.push(ctx, Task::Visit(Pair { source, ..pair }))?;
                            }
                            continue;
                        }
                        (_, Node::Union(arms) | Node::Choice(arms)) => {
                            let mut structural = false;
                            if !pair.overlap && self.has_choices(pair.source) {
                                for &arm in &arms.data {
                                    ctx.charge(1)?;
                                    structural |= matches!(
                                        (source, self.node(arm)),
                                        (Node::Tuple(_), Node::Array(_) | Node::Tuple(_))
                                            | (Node::Shape(..), Node::Shape(..) | Node::Hash(..))
                                    );
                                }
                            }
                            tasks.push(
                                ctx,
                                Task::Any(arms.data.len(), structural.then_some(pair)),
                            )?;
                            for &target in arms.data.iter().rev() {
                                tasks.push(ctx, Task::Visit(Pair { target, ..pair }))?;
                            }
                            continue;
                        }
                        (
                            Node::Atom(Atom::String | Atom::Symbol),
                            Node::Atom(Atom::String | Atom::Symbol),
                        ) if pair.keys => Relation::Accepted,
                        (Node::Symbol(_), Node::Atom(Atom::String))
                        | (Node::String(_), Node::Atom(Atom::Symbol))
                            if pair.keys =>
                        {
                            Relation::Accepted
                        }
                        (Node::String(a), Node::Symbol(b)) | (Node::Symbol(a), Node::String(b))
                            if pair.keys =>
                        {
                            if super::facts::same_bytes(ctx, a, b)? {
                                Relation::Accepted
                            } else {
                                Relation::Rejected
                            }
                        }
                        _ if self.integer_bounds(pair.source).is_some()
                            && self.integer_bounds(pair.target).is_some() =>
                        {
                            let source = self.integer_bounds(pair.source).unwrap();
                            let target = self.integer_bounds(pair.target).unwrap();
                            if source.intersection(target).is_none() {
                                Relation::Rejected
                            } else if !pair.overlap && target.contains(source) {
                                Relation::Accepted
                            } else {
                                Relation::Gradual
                            }
                        }
                        (Node::Integer(_), Node::Atom(Atom::Int))
                        | (Node::Float(_), Node::Atom(Atom::Float))
                        | (Node::Range(..), Node::Atom(Atom::Range))
                        | (Node::Regex(_), Node::Atom(Atom::Regex))
                        | (Node::String(_), Node::Atom(Atom::String))
                        | (Node::Boolean(_), Node::Atom(Atom::Bool))
                        | (Node::Symbol(_), Node::Atom(Atom::Symbol)) => Relation::Accepted,
                        (Node::Atom(Atom::Int), Node::Integer(_))
                        | (Node::Atom(Atom::Float), Node::Float(_))
                        | (Node::Atom(Atom::Range), Node::Range(..))
                        | (Node::Atom(Atom::Regex), Node::Regex(_))
                        | (Node::Atom(Atom::String), Node::String(_))
                        | (Node::Atom(Atom::Bool), Node::Boolean(_))
                        | (Node::Atom(Atom::Symbol), Node::Symbol(_)) => Relation::Gradual,
                        (Node::Array(_), Node::Array(_)) if pair.overlap => Relation::Gradual,
                        (Node::Array(source), Node::Array(target)) => {
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    source: *source,
                                    target: *target,
                                    keys: false,
                                    overlap: pair.overlap,
                                }),
                            )?;
                            continue;
                        }
                        (Node::Tuple(source), Node::Array(target)) => {
                            tasks.push(ctx, Task::All(source.data.len()))?;
                            for &source in source.data.iter().rev() {
                                tasks.push(
                                    ctx,
                                    Task::Visit(Pair {
                                        source,
                                        target: *target,
                                        keys: false,
                                        overlap: pair.overlap,
                                    }),
                                )?;
                            }
                            continue;
                        }
                        (Node::Tuple(source), Node::Tuple(target))
                            if source.data.len() == target.data.len() =>
                        {
                            tasks.push(ctx, Task::All(source.data.len()))?;
                            for (&source, &target) in source.data.iter().zip(&target.data).rev() {
                                tasks.push(
                                    ctx,
                                    Task::Visit(Pair {
                                        source,
                                        target,
                                        keys: false,
                                        overlap: pair.overlap,
                                    }),
                                )?;
                            }
                            continue;
                        }
                        (Node::Array(_), Node::Tuple(_)) => Relation::Gradual,
                        // An unparameterized hash checks only the container kind.
                        (Node::Hash(..) | Node::Shape(..), Node::Hash(key, value, _))
                            if *key == Atom::Unknown.fact() && *value == Atom::Unknown.fact() =>
                        {
                            Relation::Accepted
                        }
                        // Runtime rejects every hash, empty or not, against a key contract
                        // such as hash<int, int> before it looks at stored keys or values.
                        // Only sources with known key provenance are decided here; unknown
                        // keys keep their gradual comparison below.
                        (
                            Node::Hash(source_keys, _, _) | Node::Shape(_, _, source_keys, _),
                            Node::Hash(key, _, _),
                        ) if self.impossible_keys(*key)
                            && self.string_key(*source_keys).is_some() =>
                        {
                            Relation::Rejected
                        }
                        (Node::Hash(..), Node::Hash(..)) if pair.overlap => Relation::Gradual,
                        (Node::Hash(sk, sv, _), Node::Hash(tk, tv, _)) => {
                            tasks.push(ctx, Task::All(2))?;
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    source: *sv,
                                    target: *tv,
                                    keys: false,
                                    overlap: pair.overlap,
                                }),
                            )?;
                            tasks.push(
                                ctx,
                                Task::Visit(Pair {
                                    source: *sk,
                                    target: *tk,
                                    keys: true,
                                    overlap: pair.overlap,
                                }),
                            )?;
                            continue;
                        }
                        (Node::Shape(source, open, source_keys, _), Node::Hash(key, value, _)) => {
                            if source.data.is_empty() && !open {
                                // An empty closed shape with unknown key provenance cannot
                                // be promised to pass a key contract that rejects every
                                // hash; known keys were already rejected above.
                                if self.impossible_keys(*key) {
                                    Relation::Gradual
                                } else {
                                    Relation::Accepted
                                }
                            } else {
                                ctx.charge(source.data.len() as u64)?;
                                let count = source
                                    .data
                                    .iter()
                                    .filter(|field| !pair.overlap || !field.optional)
                                    .count();
                                tasks.push(ctx, Task::All(count + 2))?;
                                values.push(
                                    ctx,
                                    if *open {
                                        Relation::Gradual
                                    } else {
                                        Relation::Accepted
                                    },
                                )?;
                                tasks.push(
                                    ctx,
                                    Task::Visit(Pair {
                                        source: *source_keys,
                                        target: *key,
                                        keys: true,
                                        overlap: pair.overlap,
                                    }),
                                )?;
                                for field in source.data.iter().rev() {
                                    if pair.overlap && field.optional {
                                        continue;
                                    }
                                    tasks.push(
                                        ctx,
                                        Task::Visit(Pair {
                                            source: field.value,
                                            target: *value,
                                            keys: false,
                                            overlap: pair.overlap,
                                        }),
                                    )?;
                                }
                                continue;
                            }
                        }
                        (Node::Hash(_, source, _), Node::Shape(target, ..)) => {
                            ctx.charge(target.data.len() as u64)?;
                            let count = target.data.iter().filter(|field| !field.optional).count();
                            tasks.push(ctx, Task::All(count + 1))?;
                            values.push(ctx, Relation::Gradual)?;
                            for field in target.data.iter().rev().filter(|field| !field.optional) {
                                tasks.push(
                                    ctx,
                                    Task::Visit(Pair {
                                        source: *source,
                                        target: field.value,
                                        keys: false,
                                        overlap: true,
                                    }),
                                )?;
                            }
                            continue;
                        }
                        (
                            Node::Shape(source, source_open, _, _),
                            Node::Shape(target, target_open, _, _),
                        ) => {
                            let mut matched = Buffer::empty();
                            let mut result = Relation::Accepted;
                            let mut si = 0;
                            for field in &target.data {
                                ctx.charge(1)?;
                                let name = field.name.as_bytes().unwrap();
                                while si < source.data.len() {
                                    let source_name = source.data[si].name.as_bytes().unwrap();
                                    ctx.work_bytes(name.len().min(source_name.len()))?;
                                    if source_name >= name {
                                        break;
                                    }
                                    if !target_open && (!pair.overlap || !source.data[si].optional)
                                    {
                                        result = Relation::Rejected;
                                    }
                                    si += 1;
                                }
                                let found = source
                                    .data
                                    .get(si)
                                    .filter(|source| source.name.as_bytes() == Some(name));
                                if let Some(source) = found {
                                    if !pair.overlap && source.optional && !field.optional {
                                        result = Relation::Rejected;
                                    }
                                    if !pair.overlap || !source.optional || !field.optional {
                                        matched.push(
                                            ctx,
                                            Pair {
                                                source: source.value,
                                                target: field.value,
                                                keys: false,
                                                overlap: pair.overlap,
                                            },
                                        )?;
                                    }
                                    si += 1;
                                } else if !field.optional {
                                    result = all(
                                        result,
                                        if *source_open {
                                            Relation::Gradual
                                        } else {
                                            Relation::Rejected
                                        },
                                    );
                                }
                            }
                            if !target_open {
                                for field in &source.data[si..] {
                                    ctx.charge(1)?;
                                    if !pair.overlap || !field.optional {
                                        result = Relation::Rejected;
                                    }
                                }
                                if *source_open {
                                    result = all(result, Relation::Gradual);
                                }
                            }
                            tasks.push(ctx, Task::All(matched.data.len() + 1))?;
                            values.push(ctx, result)?;
                            for pair in matched.data.into_iter().rev() {
                                tasks.push(ctx, Task::Visit(pair))?;
                            }
                            continue;
                        }
                        (
                            Node::Symbol(value),
                            Node::Nominal {
                                symbols: Some(symbols),
                                ..
                            },
                        ) => {
                            let mut found = false;
                            for symbol in &symbols.data {
                                if same_bytes(ctx, value, symbol)? {
                                    found = true;
                                    break;
                                }
                            }
                            if found {
                                Relation::Accepted
                            } else {
                                Relation::Rejected
                            }
                        }
                        (
                            Node::Atom(Atom::Symbol),
                            Node::Nominal {
                                symbols: Some(_), ..
                            },
                        ) => Relation::Gradual,
                        _ => Relation::Rejected,
                    }
                }
            };
            values.push(ctx, value)?;
        }
        debug_assert_eq!(values.data.len(), 1);
        Ok(values.data[0])
    }
}

fn all(a: Relation, b: Relation) -> Relation {
    use Relation::*;
    match (a, b) {
        (Rejected, _) | (_, Rejected) => Rejected,
        (Gradual, _) | (_, Gradual) => Gradual,
        _ => Accepted,
    }
}

fn any(a: Relation, b: Relation) -> Relation {
    use Relation::*;
    match (a, b) {
        (Accepted, _) | (_, Accepted) => Accepted,
        (Gradual, _) | (_, Gradual) => Gradual,
        _ => Rejected,
    }
}
