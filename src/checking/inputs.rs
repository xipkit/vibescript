use super::{
    admission,
    calls::{Host, World},
    facts::{Atom, Callable, Fact, Facts},
};
use crate::{CallContext, Result, Value, budget::Buffer, value::Kind};

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

pub(super) struct Values<'a> {
    loaded: Buffer<Option<Loaded>>,
    hosts: Buffer<Host<'a>>,
    methods: Buffer<(&'a crate::capability::BoundMethod, usize)>,
}

impl<'a> Values<'a> {
    pub fn new() -> Self {
        Self {
            loaded: Buffer::empty(),
            hosts: Buffer::empty(),
            methods: Buffer::empty(),
        }
    }

    pub fn cached(&self, index: usize) -> Option<Loaded> {
        self.loaded.data.get(index).copied().flatten()
    }

    pub fn host<'b>(&'b self, world: &'b World<'a>, index: usize) -> Option<&'b Host<'a>> {
        if index < world.hosts.len() {
            world.hosts.get(index)
        } else {
            self.hosts.data.get(index - world.hosts.len())
        }
    }

    /// Materializes a supplied value once, retaining ordinary import failures as flow outcomes.
    pub fn read(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        world: &World<'a>,
        index: usize,
    ) -> Result<Loaded> {
        ctx.checkpoint()?;
        if let Some(value) = self.cached(index) {
            return Ok(value);
        }
        let Some(&source) = world.inputs.get(index) else {
            return Ok(Loaded::unavailable());
        };
        self.loaded.ensure(ctx, index + 1)?;
        if self.loaded.data.len() <= index {
            ctx.charge((index + 1 - self.loaded.data.len()) as u64)?;
            self.loaded.data.resize(index + 1, None);
        }
        let value = match self.admit(ctx, facts, world, source) {
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
        self.loaded.data[index] = Some(value);
        Ok(value)
    }

    fn admit(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        world: &super::calls::World<'a>,
        value: &'a Value,
    ) -> Result<admission::Admitted> {
        let mut nominal = false;
        let mut value = admission::value(ctx, facts, value, |ctx, facts, value| {
            Ok(Some(match &value.0 {
                Kind::Host(method) => {
                    let mut index = None;
                    for (previous, slot) in &self.methods.data {
                        ctx.charge(1)?;
                        if previous.same(method) {
                            index = Some(*slot);
                            break;
                        }
                    }
                    let index = if let Some(index) = index {
                        index
                    } else {
                        let index = world.hosts.len() + self.hosts.data.len();
                        let host = Host::bound(ctx, facts, method)?;
                        self.hosts.push(ctx, host)?;
                        self.methods.push(ctx, (method, index))?;
                        index
                    };
                    facts.callable(ctx, world.source_owner, Callable::Host(index))?
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
