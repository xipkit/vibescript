use super::{
    facts::{Atom, Fact, Facts, Field, InstanceKind, Node, NominalId},
    globals::layout::Layout,
    mutations::Mutation,
    scalar::Operation,
    sources::SourceId,
};
use crate::{
    CallContext, Result, Value, budget::Buffer, bytecode::Program, value::Kind as ValueKind,
};

// A class heap is a tuple of field facts indexed by allocation order. An entry that is an
// array fact summarizes every object a widening point folded into it: its element holds the
// fields of any of them, and writes through it keep the fields of the others.

fn summary(facts: &Facts, entry: Fact) -> bool {
    (0..facts.arm_count(entry)).any(|i| matches!(facts.node(facts.arm(entry, i)), Node::Array(_)))
}

/// Reads the fields of the objects an entry stands for.
fn fields(ctx: &mut CallContext, facts: &mut Facts, entry: Fact) -> Result<Fact> {
    if !summary(facts, entry) {
        return Ok(entry);
    }
    let mut values = Buffer::empty();
    for i in 0..facts.arm_count(entry) {
        ctx.charge(1)?;
        let arm = facts.arm(entry, i);
        match facts.node(arm) {
            Node::Array(element) => values.push(ctx, *element)?,
            _ => values.push(ctx, arm)?,
        }
    }
    facts.union(ctx, &values.data)
}

/// Joins the alternatives of one entry, keeping a summary a summary.
fn entry_union(ctx: &mut CallContext, facts: &mut Facts, a: Fact, b: Fact) -> Result<Fact> {
    if !summary(facts, a) && !summary(facts, b) {
        return facts.union(ctx, &[a, b]);
    }
    let a = fields(ctx, facts, a)?;
    let b = fields(ctx, facts, b)?;
    let joined = facts.union(ctx, &[a, b])?;
    facts.array(ctx, joined)
}

/// Coalesces heap alternatives into one list of entries. A shorter alternative has not
/// allocated the later objects on its path, so no reference to them exists there.
pub(super) fn entries(
    ctx: &mut CallContext,
    facts: &mut Facts,
    heap: Fact,
) -> Result<Option<Buffer<Fact>>> {
    let mut entries: Option<Buffer<Fact>> = None;
    for i in 0..facts.arm_count(heap) {
        ctx.charge(1)?;
        let arm = facts.arm(heap, i);
        if arm == Atom::Never.fact() {
            continue;
        }
        let Node::Tuple(values) = facts.node(arm) else {
            return Ok(None);
        };
        let mut copied = Buffer::empty();
        copied.extend(ctx, &values.data)?;
        let Some(entries) = &mut entries else {
            entries = Some(copied);
            continue;
        };
        for (index, value) in copied.data.into_iter().enumerate() {
            ctx.charge(1)?;
            if let Some(entry) = entries.data.get_mut(index) {
                *entry = entry_union(ctx, facts, *entry, value)?;
            } else {
                entries.push(ctx, value)?;
            }
        }
    }
    Ok(Some(entries.unwrap_or_else(Buffer::empty)))
}

/// Reads the selected objects' fields. An instance reference proves the selected object
/// exists even after heap widening.
pub(super) fn read(
    ctx: &mut CallContext,
    facts: &mut Facts,
    heap: Fact,
    index: Fact,
) -> Result<Operation> {
    let mut values = Buffer::empty();
    let mut unsupported = false;
    for i in 0..facts.arm_count(heap) {
        ctx.charge(1)?;
        match facts.node(facts.arm(heap, i)) {
            Node::Tuple(entries) => {
                if let Node::Integer(index) = facts.node(index) {
                    if let Ok(index) = usize::try_from(*index) {
                        if let Some(&value) = entries.data.get(index) {
                            values.push(ctx, value)?;
                        }
                    }
                } else {
                    values.extend(ctx, &entries.data)?;
                }
            }
            Node::Array(element) => values.push(ctx, *element)?,
            Node::Atom(Atom::Never) => (),
            _ => unsupported = true,
        }
    }
    for value in &mut values.data {
        *value = fields(ctx, facts, *value)?;
    }
    Ok(Operation {
        value: facts.union(ctx, &values.data)?,
        rejected: false,
        unsupported,
        throws: false,
    })
}

/// Writes one object's fields. A summary entry keeps the fields of the other objects it
/// stands for.
pub(super) fn write(
    ctx: &mut CallContext,
    facts: &mut Facts,
    heap: Fact,
    index: Fact,
    value: Fact,
) -> Result<Mutation> {
    let Node::Integer(position) = *facts.node(index) else {
        return facts.collection_write(ctx, heap, index, value);
    };
    let Ok(position) = usize::try_from(position) else {
        return facts.collection_write(ctx, heap, index, value);
    };
    let mut summarized = false;
    for i in 0..facts.arm_count(heap) {
        ctx.charge(1)?;
        if let Node::Tuple(entries) = facts.node(facts.arm(heap, i)) {
            if let Some(&entry) = entries.data.get(position) {
                summarized |= summary(facts, entry);
            }
        }
    }
    if !summarized || value == Atom::Never.fact() {
        return facts.collection_write(ctx, heap, index, value);
    }
    let Some(mut entries) = entries(ctx, facts, heap)? else {
        return facts.collection_write(ctx, heap, index, value);
    };
    let before = fields(ctx, facts, entries.data[position])?;
    let joined = facts.union(ctx, &[before, value])?;
    entries.data[position] = facts.array(ctx, joined)?;
    Ok(Mutation {
        receiver: facts.tuple(ctx, &entries.data)?,
        value,
        rejected: false,
        unsupported: false,
        throws: false,
    })
}

/// Maps every fact that a structure carries, such as renaming folded objects.
pub(super) type Rename<'a> = dyn FnMut(&mut CallContext, Fact) -> Result<Fact> + 'a;

/// Objects allocated at or after `base` in one class heap, summarized by the entry at
/// `target`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Fold {
    /// The heap's index among the global slots of its layout.
    pub heap: usize,
    pub base: usize,
    pub target: usize,
}

/// The global slots of every class heap in a layout.
pub(super) fn slots(ctx: &mut CallContext, layout: &Layout) -> Result<Buffer<usize>> {
    let mut heaps = Buffer::empty();
    for source in layout.sources() {
        for root in source.namespaces.clone().step_by(super::namespaces::WIDTH) {
            heaps.push(ctx, root + 2)?;
        }
    }
    Ok(heaps)
}

/// Whether a global slot of a layout holds a class heap.
pub(super) fn is_heap(ctx: &mut CallContext, layout: &Layout, global: usize) -> Result<bool> {
    for source in layout.sources() {
        ctx.charge(1)?;
        if source.namespaces.contains(&global) {
            return Ok((global - source.namespaces.start) % super::namespaces::WIDTH == 2);
        }
    }
    Ok(false)
}

/// Finds the heaps that allocated beyond an earlier state at a widening point. Objects the
/// earlier heap already held keep their identity; later ones join one summary entry, the
/// `preferred` one for a heap when the earlier state already summarizes there.
pub(super) fn folds(
    ctx: &mut CallContext,
    facts: &mut Facts,
    layout: &Layout,
    preferred: Option<(usize, usize)>,
    mut earlier: impl FnMut(&mut CallContext, usize) -> Result<Option<Fact>>,
    mut later: impl FnMut(&mut CallContext, usize) -> Result<Option<Fact>>,
) -> Result<Buffer<Fold>> {
    let mut folds = Buffer::empty();
    for heap in slots(ctx, layout)?.data {
        ctx.charge(1)?;
        let (Some(a), Some(b)) = (earlier(ctx, heap)?, later(ctx, heap)?) else {
            continue;
        };
        if a == b {
            continue;
        }
        let (Some(before), Some(after)) = (entries(ctx, facts, a)?, entries(ctx, facts, b)?) else {
            continue;
        };
        // An exact object where the earlier heap already holds a summary is also new: the
        // earlier state summarized the objects allocated from that position on.
        let mut base = before.data.len();
        for (index, (&earlier, &later)) in before.data.iter().zip(&after.data).enumerate() {
            ctx.charge(1)?;
            if summary(facts, earlier) && !summary(facts, later) {
                base = index;
                break;
            }
        }
        if after.data.len() <= base {
            continue;
        }
        let mut target = base;
        for (index, &entry) in before.data.iter().enumerate() {
            ctx.charge(1)?;
            if summary(facts, entry) {
                target = index;
                break;
            }
        }
        if let Some((_, slot)) = preferred.filter(|&(preferred, _)| preferred == heap) {
            if before
                .data
                .get(slot)
                .is_some_and(|&entry| summary(facts, entry))
            {
                target = slot;
            }
        }
        folds.push(ctx, Fold { heap, base, target })?;
    }
    Ok(folds)
}

/// Identifies the heap of a class the way instance dispatch finds its storage, for facts
/// analyzed by `program`, the code of `source`.
pub(super) fn heap_of(
    ctx: &mut CallContext,
    facts: &mut Facts,
    layout: &Layout,
    program: &Program,
    source: SourceId,
    class: Fact,
) -> Result<Option<usize>> {
    let Node::Nominal {
        identity: NominalId::Binding(owner, declaration),
        ..
    } = *facts.node(class)
    else {
        return Ok(None);
    };
    let defining = facts.source_id(ctx, owner)?;
    let code = if defining == source {
        None
    } else {
        facts.source_code(ctx, defining)?
    };
    let program = match &code {
        Some(code) => &code.program,
        None if defining == source => program,
        None => return Ok(None),
    };
    let Some(Value(ValueKind::Namespace(namespace))) = program.declarations.get(declaration) else {
        return Ok(None);
    };
    Ok(layout
        .find(ctx, defining)?
        .map(|slots| slots.namespace(namespace.definition.index) + 2))
}

/// Rewrites references to folded objects, and merges folded heap entries into their summary.
pub(super) struct Renamer<'a> {
    folds: &'a [Fold],
    layout: &'a Layout,
    program: &'a Program,
    source: SourceId,
    classes: Buffer<(Fact, Option<Fold>)>,
    memo: Buffer<(Fact, Fact)>,
    buckets: Buffer<usize>,
}

enum Task {
    Visit(Fact),
    Save(Fact),
    Array,
    Tuple(usize),
    Union(usize),
    Hash(super::facts::HashKind),
    Shape(Buffer<Field>, bool, super::facts::HashKind),
    Protected(crate::hash::Tag, super::facts::Certainty),
    Offset,
}

impl<'a> Renamer<'a> {
    /// Prepares folds for facts analyzed by `program`, the code of `source`.
    pub fn new(
        folds: &'a [Fold],
        layout: &'a Layout,
        program: &'a Program,
        source: SourceId,
    ) -> Self {
        Self {
            folds,
            layout,
            program,
            source,
            classes: Buffer::empty(),
            memo: Buffer::empty(),
            buckets: Buffer::empty(),
        }
    }

    /// Whether any fact can change.
    pub fn active(&self) -> bool {
        !self.folds.is_empty()
    }

    fn class_fold(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        class: Fact,
    ) -> Result<Option<Fold>> {
        for &(known, fold) in &self.classes.data {
            ctx.charge(1)?;
            if known == class {
                return Ok(fold);
            }
        }
        let fold = heap_of(ctx, facts, self.layout, self.program, self.source, class)?
            .and_then(|heap| self.folds.iter().copied().find(|fold| fold.heap == heap));
        self.classes.push(ctx, (class, fold))?;
        Ok(fold)
    }

    fn bucket(fact: Fact, mask: usize) -> usize {
        fact.0.wrapping_mul(0x9e37_79b9) & mask
    }

    fn remembered(&self, ctx: &mut CallContext, fact: Fact) -> Result<Option<Fact>> {
        ctx.charge(1)?;
        if self.buckets.data.is_empty() {
            return Ok(None);
        }
        let mask = self.buckets.data.len() - 1;
        let mut slot = Self::bucket(fact, mask);
        loop {
            let index = self.buckets.data[slot];
            if index == usize::MAX {
                return Ok(None);
            }
            if self.memo.data[index].0 == fact {
                return Ok(Some(self.memo.data[index].1));
            }
            ctx.charge(1)?;
            slot = (slot + 1) & mask;
        }
    }

    fn remember(&mut self, ctx: &mut CallContext, fact: Fact, value: Fact) -> Result<()> {
        if (self.memo.data.len() + 1) * 2 > self.buckets.data.len() {
            let capacity = self.buckets.data.len().max(8) * 2;
            let mut buckets = Buffer::with_capacity(ctx, capacity)?;
            ctx.charge(capacity as u64 + self.memo.data.len() as u64)?;
            buckets.data.resize(capacity, usize::MAX);
            for (index, &(key, _)) in self.memo.data.iter().enumerate() {
                let mut slot = Self::bucket(key, capacity - 1);
                while buckets.data[slot] != usize::MAX {
                    slot = (slot + 1) & (capacity - 1);
                }
                buckets.data[slot] = index;
            }
            self.buckets = buckets;
        }
        let mask = self.buckets.data.len() - 1;
        let mut slot = Self::bucket(fact, mask);
        while self.buckets.data[slot] != usize::MAX {
            ctx.charge(1)?;
            slot = (slot + 1) & mask;
        }
        self.buckets.data[slot] = self.memo.data.len();
        self.memo.push(ctx, (fact, value))
    }

    /// Renames every reference to a folded object within a fact.
    pub fn fact(&mut self, ctx: &mut CallContext, facts: &mut Facts, fact: Fact) -> Result<Fact> {
        ctx.checkpoint()?;
        if self.folds.is_empty() || !facts.growable(fact) {
            return Ok(fact);
        }
        let mut tasks = Buffer::empty();
        let mut values = Buffer::empty();
        tasks.push(ctx, Task::Visit(fact))?;
        while let Some(task) = tasks.data.pop() {
            ctx.charge(1)?;
            let value = match task {
                Task::Visit(fact) => {
                    if !facts.growable(fact) {
                        values.push(ctx, fact)?;
                        continue;
                    }
                    if let Some(value) = self.remembered(ctx, fact)? {
                        values.push(ctx, value)?;
                        continue;
                    }
                    tasks.push(ctx, Task::Save(fact))?;
                    match facts.node(fact) {
                        Node::Instance { class, slot, kind } => {
                            let (class, slot, kind) = (*class, *slot, *kind);
                            let fold = if kind == InstanceKind::Captured {
                                None
                            } else {
                                self.class_fold(ctx, facts, class)?
                            };
                            match fold {
                                Some(fold) if slot >= fold.base => facts.instance_kind(
                                    ctx,
                                    class,
                                    fold.target,
                                    InstanceKind::Folded,
                                )?,
                                _ => fact,
                            }
                        }
                        Node::Array(element) => {
                            let element = *element;
                            tasks.push(ctx, Task::Array)?;
                            tasks.push(ctx, Task::Visit(element))?;
                            continue;
                        }
                        Node::Tuple(elements) | Node::Union(elements) => {
                            let tuple = matches!(facts.node(fact), Node::Tuple(_));
                            let mut children = Buffer::empty();
                            children.extend(ctx, &elements.data)?;
                            tasks.push(
                                ctx,
                                if tuple {
                                    Task::Tuple(children.data.len())
                                } else {
                                    Task::Union(children.data.len())
                                },
                            )?;
                            for &child in children.data.iter().rev() {
                                tasks.push(ctx, Task::Visit(child))?;
                            }
                            continue;
                        }
                        Node::Hash(key, value, kind) => {
                            let (key, value, kind) = (*key, *value, *kind);
                            tasks.push(ctx, Task::Hash(kind))?;
                            tasks.push(ctx, Task::Visit(value))?;
                            tasks.push(ctx, Task::Visit(key))?;
                            continue;
                        }
                        Node::Shape(fields, open, keys, kind) => {
                            let (open, keys, kind) = (*open, *keys, *kind);
                            let mut copied = Buffer::empty();
                            let mut children = Buffer::empty();
                            for field in &fields.data {
                                ctx.charge(1)?;
                                children.push(ctx, field.value)?;
                                copied.push(
                                    ctx,
                                    Field {
                                        name: field.name.clone(),
                                        value: field.value,
                                        optional: field.optional,
                                    },
                                )?;
                            }
                            tasks.push(ctx, Task::Shape(copied, open, kind))?;
                            for &child in children.data.iter().rev() {
                                tasks.push(ctx, Task::Visit(child))?;
                            }
                            tasks.push(ctx, Task::Visit(keys))?;
                            continue;
                        }
                        Node::Protected(inner, tag, certainty) => {
                            let (inner, tag, certainty) = (*inner, *tag, *certainty);
                            tasks.push(ctx, Task::Protected(tag, certainty))?;
                            tasks.push(ctx, Task::Visit(inner))?;
                            continue;
                        }
                        Node::Offset(inner) => {
                            let inner = *inner;
                            tasks.push(ctx, Task::Offset)?;
                            tasks.push(ctx, Task::Visit(inner))?;
                            continue;
                        }
                        _ => fact,
                    }
                }
                Task::Save(fact) => {
                    let value = *values.data.last().unwrap();
                    self.remember(ctx, fact, value)?;
                    continue;
                }
                Task::Array => {
                    let element = values.data.pop().unwrap();
                    facts.array(ctx, element)?
                }
                Task::Tuple(count) | Task::Union(count) => {
                    let start = values.data.len() - count;
                    let value = if matches!(task, Task::Tuple(_)) {
                        facts.tuple(ctx, &values.data[start..])?
                    } else {
                        facts.union(ctx, &values.data[start..])?
                    };
                    values.data.truncate(start);
                    value
                }
                Task::Hash(kind) => {
                    let value = values.data.pop().unwrap();
                    let key = values.data.pop().unwrap();
                    facts.hash_kind(ctx, key, value, kind)?
                }
                Task::Shape(mut fields, open, kind) => {
                    let start = values.data.len() - fields.data.len();
                    for (field, &value) in fields.data.iter_mut().zip(&values.data[start..]) {
                        ctx.charge(1)?;
                        field.value = value;
                    }
                    values.data.truncate(start);
                    let keys = values.data.pop().unwrap();
                    facts.shape_fields(ctx, fields, open, keys, kind)?
                }
                Task::Protected(tag, certainty) => {
                    let inner = values.data.pop().unwrap();
                    facts.protected_as(ctx, inner, tag, certainty)?
                }
                Task::Offset => {
                    let inner = values.data.pop().unwrap();
                    facts.offset(ctx, inner)?
                }
            };
            values.push(ctx, value)?;
        }
        assert_eq!(values.data.len(), 1);
        Ok(values.data[0])
    }

    /// Merges the folded entries of a renamed class heap into its summary entry.
    pub fn merge(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        heap: usize,
        value: Fact,
    ) -> Result<Fact> {
        let Some(fold) = self.folds.iter().copied().find(|fold| fold.heap == heap) else {
            return Ok(value);
        };
        let Some(mut entries) = entries(ctx, facts, value)? else {
            return Ok(value);
        };
        if entries.data.len() <= fold.base {
            return Ok(value);
        }
        let mut folded = Buffer::empty();
        if fold.target < fold.base {
            let entry = fields(ctx, facts, entries.data[fold.target])?;
            folded.push(ctx, entry)?;
        }
        for index in fold.base..entries.data.len() {
            let entry = fields(ctx, facts, entries.data[index])?;
            folded.push(ctx, entry)?;
        }
        let joined = facts.union(ctx, &folded.data)?;
        let summary = facts.array(ctx, joined)?;
        entries.data.truncate(fold.base);
        if fold.target < fold.base {
            entries.data[fold.target] = summary;
        } else {
            entries.push(ctx, summary)?;
        }
        facts.tuple(ctx, &entries.data)
    }
}

/// Joins two class heaps entry by entry when their lengths differ or either holds a summary,
/// so objects keep their positions and summaries keep their fields' precision. Returns
/// `None` for heaps that ordinary tuple joins already handle.
pub(super) fn join(
    ctx: &mut CallContext,
    facts: &mut Facts,
    a: Fact,
    b: Fact,
    depth: Option<usize>,
) -> Result<Option<Fact>> {
    if a == b {
        return Ok(Some(a));
    }
    let (Some(mut before), Some(after)) = (entries(ctx, facts, a)?, entries(ctx, facts, b)?) else {
        return Ok(None);
    };
    let summarized = before.data.iter().any(|&entry| summary(facts, entry))
        || after.data.iter().any(|&entry| summary(facts, entry));
    if before.data.len() == after.data.len() && !summarized {
        return Ok(None);
    }
    // Entries sit one level below the heap, as tuple widening would treat them; a summary
    // entry's fields sit at the same level as an exact object's.
    let depth = depth.map(|depth| depth.saturating_sub(1));
    for (index, &value) in after.data.iter().enumerate() {
        ctx.charge(1)?;
        let Some(entry) = before.data.get_mut(index) else {
            before.push(ctx, value)?;
            continue;
        };
        *entry = if summary(facts, *entry) || summary(facts, value) {
            let x = fields(ctx, facts, *entry)?;
            let y = fields(ctx, facts, value)?;
            let joined = facts.joined(ctx, x, y, depth)?;
            facts.array(ctx, joined)?
        } else {
            facts.joined(ctx, *entry, value, depth)?
        };
    }
    facts.tuple(ctx, &before.data).map(Some)
}
