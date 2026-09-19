use super::*;
use crate::{
    budget::MAX_VALUE_DEPTH,
    checking::{
        facts::{Field, HashKind, InstanceKind},
        sources::SourceId,
    },
    code::Code,
    objects::Instance,
};

pub(in crate::checking) struct Captures {
    pub sources: Buffer<Source>,
    pub objects: Buffer<Object>,
    pub batches: Buffer<Buffer<SourceId>>,
    pub entry: Buffer<SourceId>,
}

pub(in crate::checking) struct Source {
    pub owner: usize,
    pub id: SourceId,
    pub environment: Option<u64>,
}

pub(in crate::checking) struct Object {
    identity: u64,
    pub class: Option<Fact>,
    source: Option<SourceId>,
    environment: Option<u64>,
    children: Buffer<Reference>,
    pub fields: Fact,
}

#[derive(Clone, Copy)]
enum Reference {
    Source(SourceId),
    Object(u64),
}

enum Visit {
    Value(Value, Option<usize>),
    Reference(Reference),
}

struct Snapshot {
    object: usize,
    fields: Buffer<(Value, Value)>,
}

impl Captures {
    pub fn new() -> Self {
        Self {
            sources: Buffer::empty(),
            objects: Buffer::empty(),
            batches: Buffer::empty(),
            entry: Buffer::empty(),
        }
    }

    fn object(&self, ctx: &mut CallContext, identity: u64) -> Result<Option<usize>> {
        for (index, object) in self.objects.data.iter().enumerate() {
            ctx.charge(1)?;
            if object.identity == identity {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    pub fn environment(&self, ctx: &mut CallContext, source: usize) -> Result<Option<Fact>> {
        let Some(identity) = self.sources.data[source].environment else {
            return Ok(None);
        };
        let object = self.object(ctx, identity)?.ok_or_else(|| {
            crate::Error::new(crate::ErrorKind::Runtime, "missing captured environment")
        })?;
        Ok(Some(self.objects.data[object].fields))
    }

    pub fn scoped(&self, ctx: &mut CallContext) -> Result<bool> {
        ctx.charge(self.sources.data.len() as u64)?;
        Ok(self
            .sources
            .data
            .iter()
            .any(|source| source.environment.is_some()))
    }

    pub(super) fn value(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        instance: &Instance,
    ) -> Result<Option<Fact>> {
        let Some(index) = self.object(ctx, instance.checking_id())? else {
            return Ok(None);
        };
        let class = self.objects.data[index]
            .class
            .unwrap_or(Atom::Unknown.fact());
        facts
            .instance_kind(ctx, class, index, InstanceKind::Captured)
            .map(Some)
    }

    fn source(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        code: &Arc<Code>,
        environment: Option<&Instance>,
        needed: &mut Buffer<SourceId>,
    ) -> Result<SourceId> {
        let owner = facts.source_owner(ctx, code, environment)?;
        let id = facts.source_id(ctx, owner)?;
        ctx.charge(self.sources.data.len() as u64 + needed.data.len() as u64 + 1)?;
        if !self.sources.data.iter().any(|source| source.id == id) {
            self.sources.push(
                ctx,
                Source {
                    owner,
                    id,
                    environment: environment.map(Instance::checking_id),
                },
            )?;
        }
        if !needed.data.contains(&id) {
            needed.push(ctx, id)?;
        }
        Ok(id)
    }

    fn namespace(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        namespace: &crate::namespace::Namespace,
        needed: &mut Buffer<SourceId>,
    ) -> Result<Option<SourceId>> {
        if let Some(code) = namespace.owner.clone().or_else(|| {
            namespace
                .definition
                .owner
                .get()
                .and_then(std::sync::Weak::upgrade)
        }) {
            return self
                .source(ctx, facts, &code, namespace.environment.as_deref(), needed)
                .map(Some);
        }
        Ok(None)
    }

    pub fn eager(&mut self, ctx: &mut CallContext, batch: usize) -> Result<()> {
        for &source in &self.batches.data[batch].data {
            ctx.charge(self.entry.data.len() as u64 + 1)?;
            if !self.entry.data.contains(&source) {
                self.entry.push(ctx, source)?;
            }
        }
        Ok(())
    }

    fn discover(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        value: &Value,
    ) -> Result<(Buffer<Snapshot>, Buffer<SourceId>)> {
        let mut snapshots = Buffer::empty();
        let mut needed = Buffer::empty();
        let mut pending = Buffer::empty();
        let mut seen = Buffer::empty();
        let mut containers = Buffer::empty();
        let mut heights = Buffer::empty();
        pending.push(ctx, Visit::Value(value.clone(), None))?;
        while let Some(visit) = pending.data.pop() {
            ctx.charge(1)?;
            let (value, parent) = match visit {
                Visit::Value(value, parent) => (value, parent),
                Visit::Reference(Reference::Source(source)) => {
                    ctx.charge(needed.data.len() as u64 + 1)?;
                    if !needed.data.contains(&source) {
                        needed.push(ctx, source)?;
                    }
                    continue;
                }
                Visit::Reference(Reference::Object(identity)) => {
                    ctx.charge(seen.data.len() as u64 + 1)?;
                    if !seen.data.contains(&identity) {
                        seen.push(ctx, identity)?;
                        let object = self.object(ctx, identity)?.unwrap();
                        self.revisit(ctx, object, &mut pending)?;
                    }
                    continue;
                }
            };
            if value.depth() > MAX_VALUE_DEPTH {
                return ctx.guard(crate::ErrorKind::Recursion, "value nesting too deep");
            }
            match &value.0 {
                Kind::Instance(instance) => {
                    let identity = instance.checking_id();
                    self.child(ctx, parent, Reference::Object(identity))?;
                    ctx.charge(seen.data.len() as u64 + 1)?;
                    if seen.data.contains(&identity) {
                        continue;
                    }
                    seen.push(ctx, identity)?;
                    let source = self.namespace(ctx, facts, instance.class(), &mut needed)?;
                    // Identity precedes fields, so cycles never require recursive fact nodes.
                    let object = if let Some(index) = self.object(ctx, identity)? {
                        self.revisit(ctx, index, &mut pending)?;
                        continue;
                    } else {
                        let class = Values::nominal(ctx, facts, instance.class())?;
                        let index = self.objects.data.len();
                        self.objects.push(
                            ctx,
                            Object {
                                identity,
                                class,
                                source,
                                environment: instance
                                    .class()
                                    .environment
                                    .as_ref()
                                    .map(|environment| environment.checking_id()),
                                children: Buffer::empty(),
                                fields: Atom::Never.fact(),
                            },
                        )?;
                        index
                    };
                    if let Some(environment) = &instance.class().environment {
                        pending.push(
                            ctx,
                            Visit::Value(Value(Kind::Instance(environment.clone())), None),
                        )?;
                    }
                    if self.objects.data[object].fields == Atom::Never.fact() {
                        let fields = crate::objects::bindings(ctx, instance)?;
                        for (_, value) in fields.data.iter().rev() {
                            pending.push(ctx, Visit::Value(value.clone(), Some(object)))?;
                        }
                        snapshots.push(ctx, Snapshot { object, fields })?;
                    }
                }
                Kind::Namespace(namespace) => {
                    if let Some(source) = self.namespace(ctx, facts, namespace, &mut needed)? {
                        self.child(ctx, parent, Reference::Source(source))?;
                    }
                    if let Some(environment) = &namespace.environment {
                        self.child(ctx, parent, Reference::Object(environment.checking_id()))?;
                        pending.push(
                            ctx,
                            Visit::Value(Value(Kind::Instance(environment.clone())), None),
                        )?;
                    }
                }
                Kind::Function(function) => {
                    let source = self.source(
                        ctx,
                        facts,
                        &function.code,
                        Some(&function.environment),
                        &mut needed,
                    )?;
                    self.child(ctx, parent, Reference::Source(source))?;
                    self.child(
                        ctx,
                        parent,
                        Reference::Object(function.environment.checking_id()),
                    )?;
                    pending.push(
                        ctx,
                        Visit::Value(Value(Kind::Instance(function.environment.clone())), None),
                    )?;
                }
                Kind::Array(array) => {
                    if seen_container(ctx, &mut containers, &mut heights, parent, &value)? {
                        continue;
                    }
                    for value in array.buffer.data.iter().rev() {
                        pending.push(ctx, Visit::Value(value.clone(), parent))?;
                    }
                }
                Kind::Hash(hash) => {
                    if seen_container(ctx, &mut containers, &mut heights, parent, &value)? {
                        continue;
                    }
                    for (key, value) in hash.buffer.data.iter().rev() {
                        pending.push(ctx, Visit::Value(value.clone(), parent))?;
                        pending.push(ctx, Visit::Value(key.clone(), parent))?;
                    }
                }
                _ => (),
            }
        }
        Ok((snapshots, needed))
    }

    fn child(
        &mut self,
        ctx: &mut CallContext,
        parent: Option<usize>,
        child: Reference,
    ) -> Result<()> {
        if let Some(parent) = parent {
            self.objects.data[parent].children.push(ctx, child)?;
        }
        Ok(())
    }

    fn revisit(
        &self,
        ctx: &mut CallContext,
        object: usize,
        pending: &mut Buffer<Visit>,
    ) -> Result<()> {
        let object = &self.objects.data[object];
        if let Some(environment) = object.environment {
            pending.push(ctx, Visit::Reference(Reference::Object(environment)))?;
        }
        for &child in object.children.data.iter().rev() {
            pending.push(ctx, Visit::Reference(child))?;
        }
        if let Some(source) = object.source {
            pending.push(ctx, Visit::Reference(Reference::Source(source)))?;
        }
        Ok(())
    }
}

fn seen_container(
    ctx: &mut CallContext,
    containers: &mut Buffer<(Option<usize>, Value, usize)>,
    heights: &mut Buffer<usize>,
    parent: Option<usize>,
    value: &Value,
) -> Result<bool> {
    // A container cannot share storage with an ancestor of greater height.
    // Index by height so deep inputs do not rescan every earlier ancestor.
    let height = value.depth();
    if heights.data.len() <= height {
        ctx.charge((height + 1 - heights.data.len()) as u64)?;
        heights.ensure(ctx, height + 1)?;
        heights.data.resize(height + 1, usize::MAX);
    }
    let mut index = heights.data[height];
    while index != usize::MAX {
        ctx.charge(1)?;
        let (previous_parent, previous, next) = &containers.data[index];
        let same = match (&previous.0, &value.0) {
            (Kind::Array(a), Kind::Array(b)) => Arc::ptr_eq(a, b),
            (Kind::Hash(a), Kind::Hash(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if *previous_parent == parent && same {
            return Ok(true);
        }
        index = *next;
    }
    let index = containers.data.len();
    containers.push(ctx, (parent, value.clone(), heights.data[height]))?;
    heights.data[height] = index;
    Ok(false)
}

impl Values {
    pub(in crate::checking) fn admit(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        world: &World<'_>,
        value: &Value,
    ) -> Result<(admission::Admitted, Option<usize>)> {
        let previous_objects = self.captured.objects.data.len();
        let previous_sources = self.captured.sources.data.len();
        let result = (|| {
            let (snapshots, needed) = self.captured.discover(ctx, facts, value)?;
            let mut incomplete = false;
            for snapshot in snapshots.data {
                let mut fields = Buffer::empty();
                for (name, value) in snapshot.fields.data {
                    let value = self.describe(ctx, facts, world, &value)?;
                    incomplete |= value.incomplete;
                    let name = ctx.bytes(name.as_bytes().unwrap())?;
                    fields.push(
                        ctx,
                        Field {
                            name,
                            value: value.value,
                            optional: false,
                        },
                    )?;
                }
                self.captured.objects.data[snapshot.object].fields =
                    facts.shape_fields(ctx, fields, false, Atom::String.fact(), HashKind::Plain)?;
            }
            let mut value = self.describe(ctx, facts, world, value)?;
            value.incomplete |= incomplete;
            let batch = if needed.data.is_empty() {
                None
            } else {
                let index = self.captured.batches.data.len();
                self.captured.batches.push(ctx, needed)?;
                Some(index)
            };
            Ok((value, batch))
        })();
        if result.is_err() {
            self.captured.objects.data.truncate(previous_objects);
            self.captured.sources.data.truncate(previous_sources);
        }
        result
    }
}
