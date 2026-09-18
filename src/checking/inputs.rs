use super::{
    admission,
    calls::{Host, World},
    facts::{Atom, Callable, Fact, Facts},
};
use crate::{CallContext, Result, Value, budget::Buffer, value::Kind};
use std::sync::Arc;

#[derive(Clone, Copy, Debug)]
pub(super) struct Loaded {
    pub value: Fact,
    pub incomplete: bool,
    pub throws: u8,
}

impl Loaded {
    pub fn unavailable() -> Self {
        Self {
            value: Atom::Never.fact(),
            incomplete: true,
            throws: 0,
        }
    }
}

pub(super) struct Values {
    pub writers: Option<[bool; 2]>,
    sources: Buffer<Source>,
}

struct Source {
    owner: usize,
    registered: usize,
    loaded: Buffer<Option<Loaded>>,
    methods: Buffer<Method>,
}

struct Method {
    descriptor: Arc<crate::capability::BoundMethod>,
    host: Host,
}

impl Values {
    pub fn new() -> Self {
        Self {
            writers: None,
            sources: Buffer::empty(),
        }
    }

    fn find_source(&self, ctx: &mut CallContext, world: &World<'_>) -> Result<Option<usize>> {
        ctx.checkpoint()?;
        for (index, source) in self.sources.data.iter().enumerate() {
            ctx.charge(1)?;
            if source.owner == world.source_owner {
                if source.registered != world.hosts.len() {
                    return Err(crate::Error::new(
                        crate::ErrorKind::Runtime,
                        "checker source host table changed",
                    ));
                }
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    fn source(&mut self, ctx: &mut CallContext, world: &World<'_>) -> Result<usize> {
        if let Some(index) = self.find_source(ctx, world)? {
            return Ok(index);
        }
        let index = self.sources.data.len();
        self.sources.push(
            ctx,
            Source {
                owner: world.source_owner,
                registered: world.hosts.len(),
                loaded: Buffer::empty(),
                methods: Buffer::empty(),
            },
        )?;
        Ok(index)
    }

    /// Selects metadata in the defining source's registered and admitted host tables.
    pub fn host<'a>(
        &'a self,
        ctx: &mut CallContext,
        world: &'a World<'_>,
        index: usize,
    ) -> Result<Option<&'a Host>> {
        ctx.charge(1)?;
        let source = self.find_source(ctx, world)?;
        if index < world.hosts.len() {
            Ok(world.hosts.get(index))
        } else {
            let Some(source) = source else {
                return Ok(None);
            };
            Ok(self.sources.data[source]
                .methods
                .data
                .get(index - world.hosts.len())
                .map(|method| &method.host))
        }
    }

    /// Materializes a supplied value once, retaining ordinary import failures as flow outcomes.
    pub fn read(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        world: &World<'_>,
        index: usize,
    ) -> Result<Loaded> {
        ctx.checkpoint()?;
        let Some(source) = world.inputs.get(index) else {
            return Ok(Loaded::unavailable());
        };
        let home = self.source(ctx, world)?;
        let loaded = &mut self.sources.data[home].loaded;
        if let Some(value) = loaded.data.get(index).copied().flatten() {
            return Ok(value);
        }
        loaded.ensure(ctx, index + 1)?;
        if loaded.data.len() <= index {
            ctx.charge((index + 1 - loaded.data.len()) as u64)?;
            loaded.data.resize(index + 1, None);
        }
        let value = match self.argument(ctx, facts, world, source) {
            Ok(value) => Loaded {
                value: value.value,
                incomplete: value.incomplete,
                throws: 0,
            },
            Err(error) => {
                ctx.checkpoint()?;
                let Some(class) = error.class() else {
                    return Err(error);
                };
                Loaded {
                    value: Atom::Never.fact(),
                    incomplete: false,
                    throws: 1 << class as u8,
                }
            }
        };
        self.sources.data[home].loaded.data[index] = Some(value);
        Ok(value)
    }

    /// Admits an eager call argument, preserving entry errors outside script handlers.
    pub fn argument(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        world: &World<'_>,
        value: &Value,
    ) -> Result<admission::Admitted> {
        let mut nominal = false;
        let mut value = admission::value(ctx, facts, value, |ctx, facts, value| {
            Ok(Some(match &value.0 {
                Kind::Host(method) => {
                    let home = self.source(ctx, world)?;
                    let methods = &mut self.sources.data[home].methods;
                    let mut index = None;
                    for (slot, previous) in methods.data.iter().enumerate() {
                        ctx.charge(1)?;
                        if previous.descriptor.same(method) {
                            index = Some(slot);
                            break;
                        }
                    }
                    let index = if let Some(index) = index {
                        index
                    } else {
                        let index = methods.data.len();
                        let host = Host::bound(ctx, facts, method)?;
                        methods.push(
                            ctx,
                            Method {
                                descriptor: method.clone(),
                                host,
                            },
                        )?;
                        index
                    };
                    facts.callable(
                        ctx,
                        world.source_owner,
                        Callable::Host(world.hosts.len() + index),
                    )?
                }
                Kind::Function(function) => {
                    let owner =
                        facts.source_owner(ctx, &function.code, Some(&function.environment))?;
                    facts.callable(ctx, owner, Callable::Function(function.index))?
                }
                Kind::Namespace(namespace) => {
                    nominal = true;
                    let Some(value) = Self::nominal(ctx, facts, namespace)? else {
                        return Ok(None);
                    };
                    facts.type_value(ctx, value)?
                }
                Kind::Instance(instance) => {
                    nominal = true;
                    let Some(value) = Self::nominal(ctx, facts, instance.class())? else {
                        return Ok(None);
                    };
                    value
                }
                _ => return Ok(None),
            }))
        })?;
        value.incomplete |= nominal;
        Ok(value)
    }

    fn nominal(
        ctx: &mut CallContext,
        facts: &mut Facts,
        namespace: &crate::namespace::Namespace,
    ) -> Result<Option<Fact>> {
        let code = namespace.owner.clone().or_else(|| {
            namespace
                .definition
                .owner
                .get()
                .and_then(std::sync::Weak::upgrade)
        });
        let Some(code) = code else { return Ok(None) };
        for (index, declaration) in code.program.declarations.iter().enumerate() {
            ctx.charge(1)?;
            if let Kind::Namespace(declaration) = &declaration.0 {
                if std::sync::Arc::ptr_eq(&declaration.definition, &namespace.definition) {
                    let owner = facts.source_owner(ctx, &code, namespace.environment.as_deref())?;
                    return facts
                        .nominal(
                            ctx,
                            owner,
                            index,
                            namespace.definition.name.as_bytes(),
                            None,
                        )
                        .map(Some);
                }
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests;
