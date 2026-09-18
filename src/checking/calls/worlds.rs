use super::*;
use crate::{budget::Charge, checking::environment::Environment};

struct Prepared {
    source: SourceId,
    environment: Environment,
    layouts: Layouts,
    _charge: Option<Charge>,
}

#[derive(Clone)]
pub(super) struct Handle<'a>(Metadata<'a>);

#[derive(Clone)]
enum Metadata<'a> {
    Borrowed(View<'a>),
    Owned(Arc<Prepared>),
}

#[derive(Clone, Copy)]
pub(super) struct View<'a> {
    pub source: SourceId,
    pub world: World<'a>,
    pub layouts: &'a Layouts,
}

impl<'a> Handle<'a> {
    /// Couples a caller-owned source view with its checked lexical layout.
    pub fn borrowed(
        ctx: &mut CallContext,
        facts: &Facts,
        world: World<'a>,
        layouts: &'a Layouts,
    ) -> Result<Self> {
        ctx.checkpoint()?;
        if world.source_owner != layouts.source_owner {
            return Err(crate::Error::new(
                crate::ErrorKind::Runtime,
                "checker layout belongs to a different source",
            ));
        }
        Ok(Self(Metadata::Borrowed(View {
            source: facts.source_id(ctx, world.source_owner)?,
            world,
            layouts,
        })))
    }

    /// Owns immutable source metadata and its lexical layout for queued analysis.
    pub fn owned(ctx: &mut CallContext, facts: &Facts, environment: Environment) -> Result<Self> {
        ctx.checkpoint()?;
        if !environment.incomplete.data.is_empty() {
            return Err(crate::Error::new(
                crate::ErrorKind::Runtime,
                "checker source preparation is incomplete",
            ));
        }
        let world = environment.world();
        let source = facts.source_id(ctx, world.source_owner)?;
        let layouts = Layouts::new(ctx, world.program, world.source_owner)?;
        let charge = ctx.reserve(size_of::<Prepared>() + 2 * size_of::<usize>())?;
        Ok(Self(Metadata::Owned(Arc::new(Prepared {
            source,
            environment,
            layouts,
            _charge: charge,
        }))))
    }

    /// Borrows a body view from a handle kept outside the mutable scheduler.
    pub fn view(&self) -> View<'_> {
        match &self.0 {
            Metadata::Borrowed(view) => *view,
            Metadata::Owned(prepared) => View {
                source: prepared.source,
                world: prepared.environment.world(),
                layouts: &prepared.layouts,
            },
        }
    }
}

pub(super) struct Registered<'a> {
    handle: Handle<'a>,
    pub functions: Buffer<bool>,
    pub entry_failures: Buffer<Failure>,
}

pub(super) struct Registry<'a> {
    pub entries: Buffer<Registered<'a>>,
}

impl<'a> Registry<'a> {
    pub fn new() -> Self {
        Self {
            entries: Buffer::empty(),
        }
    }

    /// Publishes one immutable source and its separate function visitation table.
    pub fn insert(&mut self, ctx: &mut CallContext, handle: Handle<'a>) -> Result<usize> {
        ctx.checkpoint()?;
        let view = handle.view();
        for entry in &self.entries.data {
            ctx.charge(1)?;
            if entry.handle.view().source == view.source {
                return Err(crate::Error::new(
                    crate::ErrorKind::Runtime,
                    "checker source is already prepared",
                ));
            }
        }
        let mut functions = Buffer::with_capacity(ctx, view.world.program.functions.len())?;
        ctx.charge(view.world.program.functions.len() as u64)?;
        functions
            .data
            .resize(view.world.program.functions.len(), false);
        let index = self.entries.data.len();
        self.entries.push(
            ctx,
            Registered {
                handle,
                functions,
                entry_failures: Buffer::empty(),
            },
        )?;
        Ok(index)
    }

    /// Clones a source handle without borrowing the registry during body analysis.
    pub fn get(&self, ctx: &mut CallContext, source: SourceId) -> Result<(usize, Handle<'a>)> {
        ctx.checkpoint()?;
        for (index, entry) in self.entries.data.iter().enumerate() {
            ctx.charge(1)?;
            if entry.handle.view().source == source {
                return Ok((index, entry.handle.clone()));
            }
        }
        Err(crate::Error::new(
            crate::ErrorKind::Runtime,
            "checker source is not prepared",
        ))
    }
}

#[cfg(test)]
mod tests;
