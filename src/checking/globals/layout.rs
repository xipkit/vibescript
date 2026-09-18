use super::*;
use crate::checking::sources::SourceId;
use crate::{Value, budget::Charge, builtin::Global};
use std::{ops::Range, sync::Arc};

#[derive(Clone, Copy, Debug)]
pub(in crate::checking) struct Initial {
    pub value: Fact,
    pub missing: bool,
}

#[derive(Debug)]
pub(in crate::checking) struct Source {
    pub id: SourceId,
    pub receiving: SourceId,
    pub globals: Buffer<usize>,
    pub roots: Buffer<usize>,
    pub files: Range<usize>,
    pub declarations: Range<usize>,
    pub namespaces: Range<usize>,
    _charge: Option<Charge>,
}

impl Source {
    pub fn namespace(&self, module: usize) -> usize {
        let slot = self.namespaces.start + module * super::super::namespaces::WIDTH;
        assert!(slot < self.namespaces.end);
        slot
    }

    pub fn original(&self, ctx: &mut CallContext, slot: usize) -> Result<Option<usize>> {
        ctx.charge(self.globals.data.len() as u64 + 1)?;
        Ok(self.globals.data.iter().position(|&value| value == slot))
    }

    pub fn root(&self, ctx: &mut CallContext, slot: usize) -> Result<Option<usize>> {
        ctx.charge(self.roots.data.len() as u64 + 1)?;
        Ok(self.roots.data.iter().position(|&value| value == slot))
    }
}

#[derive(Debug)]
struct Lineage {
    _charge: Option<Charge>,
}

#[derive(Debug)]
struct Data {
    lineage: Arc<Lineage>,
    sources: Buffer<Arc<Source>>,
    initial: Buffer<Initial>,
    builtins: Buffer<(SourceId, Global, usize)>,
    roots: Buffer<(SourceId, Value, usize)>,
    _charge: Option<Charge>,
}

/// Immutable versions share one append-only address space owned by the scheduler.
#[derive(Clone, Debug, Default)]
pub(in crate::checking) struct Layout(Option<Arc<Data>>);

impl Layout {
    pub fn len(&self) -> usize {
        self.0.as_ref().map_or(0, |data| data.initial.data.len())
    }

    pub fn version(&self) -> usize {
        self.0.as_ref().map_or(0, |data| data.sources.data.len())
    }

    pub fn same(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }

    pub fn compatible(&self, other: &Self) -> bool {
        match (&self.0, &other.0) {
            (Some(a), Some(b)) => Arc::ptr_eq(&a.lineage, &b.lineage),
            _ => true,
        }
    }

    pub fn latest(&self, ctx: &mut CallContext, other: &Self) -> Result<Self> {
        ctx.checkpoint()?;
        if !self.compatible(other) {
            return Err(crate::Error::new(
                crate::ErrorKind::Runtime,
                "checker state belongs to a different address space",
            ));
        }
        Ok(if self.version() >= other.version() {
            self
        } else {
            other
        }
        .clone())
    }

    pub fn sources(&self) -> &[Arc<Source>] {
        self.0.as_ref().map_or(&[], |data| &data.sources.data)
    }

    pub fn find(&self, ctx: &mut CallContext, source: SourceId) -> Result<Option<Arc<Source>>> {
        ctx.checkpoint()?;
        for entry in self.sources() {
            ctx.charge(1)?;
            if entry.id == source {
                return Ok(Some(entry.clone()));
            }
        }
        Ok(None)
    }

    pub fn source(&self, ctx: &mut CallContext, source: SourceId) -> Result<Arc<Source>> {
        self.find(ctx, source)?.ok_or_else(|| {
            crate::Error::new(
                crate::ErrorKind::Runtime,
                "checker source state is not prepared",
            )
        })
    }

    pub(in crate::checking) fn initial(&self, index: usize) -> Initial {
        self.0.as_ref().unwrap().initial.data[index]
    }
}

pub(in crate::checking) struct Definition<'a> {
    pub source: SourceId,
    pub owner: usize,
    pub program: &'a Program,
    pub files: &'a super::super::file_bindings::Layout,
    pub roots: &'a [super::super::calls::Root],
    pub receiving: Option<SourceId>,
}

/// The sole allocator prevents branches from assigning different meanings to one slot.
pub(in crate::checking) struct Storage {
    pub layout: Layout,
}

impl Storage {
    pub fn new() -> Self {
        Self {
            layout: Layout::default(),
        }
    }

    pub fn prepare(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        definition: Definition<'_>,
    ) -> Result<Layout> {
        ctx.checkpoint()?;
        let Definition {
            source,
            owner,
            program,
            files,
            roots,
            receiving,
        } = definition;
        let receiving = receiving
            .map(|source| {
                self.layout
                    .source(ctx, source)
                    .map(|source| source.receiving)
            })
            .transpose()?;
        if let Some(existing) = self.layout.find(ctx, source)? {
            if receiving.is_some_and(|receiving| existing.receiving != receiving) {
                return Err(crate::Error::new(
                    crate::ErrorKind::Runtime,
                    "checker source already has a receiving environment",
                ));
            }
            return Ok(self.layout.clone());
        }
        let receiving = receiving.unwrap_or(source);
        let lineage = if let Some(previous) = &self.layout.0 {
            previous.lineage.clone()
        } else {
            let charge = ctx.reserve(size_of::<Lineage>() + 2 * size_of::<usize>())?;
            Arc::new(Lineage { _charge: charge })
        };
        let mut data = Data {
            lineage,
            sources: Buffer::empty(),
            initial: Buffer::empty(),
            builtins: Buffer::empty(),
            roots: Buffer::empty(),
            _charge: ctx.reserve(size_of::<Data>() + 2 * size_of::<usize>())?,
        };
        if let Some(previous) = &self.layout.0 {
            data.sources.extend(ctx, &previous.sources.data)?;
            data.initial.extend(ctx, &previous.initial.data)?;
            data.builtins.extend(ctx, &previous.builtins.data)?;
            data.roots.extend(ctx, &previous.roots.data)?;
        }
        let mut mapped = Source {
            id: source,
            receiving,
            globals: Buffer::empty(),
            roots: Buffer::empty(),
            files: 0..0,
            declarations: 0..0,
            namespaces: 0..0,
            _charge: ctx.reserve(size_of::<Source>() + 2 * size_of::<usize>())?,
        };
        for (global, value) in &program.globals {
            ctx.charge(data.builtins.data.len() as u64 + 1)?;
            let slot = if let Some((_, _, slot)) = data
                .builtins
                .data
                .iter()
                .find(|(root, candidate, _)| *root == receiving && candidate == global)
            {
                *slot
            } else {
                let value = builtins::global(ctx, facts, value)?;
                let slot = data.push(ctx, value, false)?;
                data.builtins.push(ctx, (receiving, *global, slot))?;
                slot
            };
            mapped.globals.push(ctx, slot)?;
        }
        for root in roots {
            let mut previous = None;
            for (receiving_source, name, slot) in &data.roots.data {
                ctx.charge(1)?;
                if *receiving_source == receiving {
                    let name = name.as_bytes().unwrap();
                    let requested = root.name.as_bytes().unwrap();
                    ctx.work_bytes(name.len().max(requested.len()))?;
                    if name == requested {
                        previous = Some(*slot);
                        break;
                    }
                }
            }
            let slot = if let Some(slot) = previous {
                slot
            } else {
                let slot = data.push(ctx, root.value, root.missing)?;
                data.roots.push(ctx, (receiving, root.name.clone(), slot))?;
                slot
            };
            mapped.roots.push(ctx, slot)?;
        }
        let start = data.initial.data.len();
        for _ in &files.names.data {
            data.push(ctx, Atom::Never.fact(), true)?;
        }
        mapped.files = start..data.initial.data.len();
        let start = data.initial.data.len();
        for declaration in &program.declarations {
            let value = match &declaration.0 {
                crate::value::Kind::Namespace(namespace) => super::super::namespaces::value(
                    ctx,
                    facts,
                    program,
                    owner,
                    namespace.definition.index,
                )?,
                crate::value::Kind::Enum(_) => facts.enumeration(ctx, declaration)?,
                _ => unreachable!(),
            };
            data.push(ctx, value, false)?;
        }
        mapped.declarations = start..data.initial.data.len();
        let start = data.initial.data.len();
        for (module, definition) in program.namespaces.iter().enumerate() {
            let fields = super::super::namespaces::initial(ctx, facts, program, owner, module)?;
            let initialized = facts.boolean(ctx, definition.body.is_none())?;
            let instances = facts.tuple(ctx, &[])?;
            for value in [fields, initialized, instances, Atom::Never.fact()] {
                data.push(ctx, value, false)?;
            }
        }
        mapped.namespaces = start..data.initial.data.len();
        data.sources.push(ctx, Arc::new(mapped))?;
        self.layout = Layout(Some(Arc::new(data)));
        Ok(self.layout.clone())
    }
}

impl Data {
    fn push(&mut self, ctx: &mut CallContext, value: Fact, missing: bool) -> Result<usize> {
        ctx.charge(1)?;
        let slot = self.initial.data.len();
        self.initial.push(ctx, Initial { value, missing })?;
        Ok(slot)
    }
}
