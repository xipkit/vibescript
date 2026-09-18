use super::{
    admission,
    arguments::Input,
    calls::{self, Analysis, Host, Target, World},
    facts::{Atom, Callable, Fact, Facts},
};
use crate::{CallContext, CallOptions, Result, Script, Value, budget::Buffer, value::Kind};

#[derive(Debug)]
pub(super) enum Incomplete {
    Initializer(usize),
    StrictGlobals,
    File,
    Capability(Value),
    Root(Value),
}

/// A metadata snapshot for analysis; this does not bind grants or execute initializers.
pub(super) struct Environment<'a> {
    script: &'a Script,
    owner: usize,
    contracts: Buffer<Fact>,
    hosts: Buffer<Host<'a>>,
    methods: Buffer<(&'a crate::capability::BoundMethod, usize)>,
    globals: Buffer<(Value, Target)>,
    pub incomplete: Buffer<Incomplete>,
}

impl<'a> Environment<'a> {
    /// Reads compiled registrations and supplied values without invoking host code.
    pub fn new(
        ctx: &mut CallContext,
        facts: &mut Facts,
        script: &'a Script,
        options: &'a CallOptions,
    ) -> Result<Self> {
        ctx.checkpoint()?;
        let code = &script.inner.code;
        let mut result = Self {
            script,
            owner: facts.source_owner(ctx, code, None)?,
            contracts: Buffer::empty(),
            hosts: Buffer::empty(),
            methods: Buffer::empty(),
            globals: Buffer::empty(),
            incomplete: Buffer::empty(),
        };
        for registered in &code.hosts {
            let host = Host::registered(ctx, facts, registered)?;
            result.hosts.push(ctx, host)?;
        }
        for ty in &code.program.types {
            let contract = facts.annotation(ctx, ty, |_, _| Ok(None))?;
            result.contracts.push(ctx, contract)?;
        }
        if code.program.file {
            result.incomplete.push(ctx, Incomplete::File)?;
        }
        for namespace in &code.program.namespaces {
            ctx.charge(1)?;
            if let Some(body) = namespace.body {
                result.incomplete.push(ctx, Incomplete::Initializer(body))?;
            }
        }
        if script.inner.strict_effects && !options.globals.is_empty() {
            result.incomplete.push(ctx, Incomplete::StrictGlobals)?;
        }
        for capability in &options.capabilities {
            let name = ctx.bytes(capability.name.as_bytes())?;
            result
                .incomplete
                .push(ctx, Incomplete::Capability(name.clone()))?;
            result.bind(ctx, name, Atom::Unknown.fact())?;
        }
        for (name, value) in &options.globals {
            let value = result.admit(ctx, facts, value)?;
            let name = ctx.bytes(name.as_bytes())?;
            if value.incomplete {
                result
                    .incomplete
                    .push(ctx, Incomplete::Root(name.clone()))?;
            }
            result.bind(ctx, name, value.value)?;
        }
        Ok(result)
    }

    fn bind(&mut self, ctx: &mut CallContext, name: Value, value: Fact) -> Result<()> {
        for (key, target) in &mut self.globals.data {
            if super::facts::same_bytes(ctx, key, &name)? {
                *target = Target::Value(value);
                return Ok(());
            }
        }
        self.globals.push(ctx, (name, Target::Value(value)))
    }

    fn admit(
        &mut self,
        ctx: &mut CallContext,
        facts: &mut Facts,
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
                        let index = self.hosts.data.len();
                        let host = Host::bound(ctx, facts, method)?;
                        self.hosts.push(ctx, host)?;
                        self.methods.push(ctx, (method, index))?;
                        index
                    };
                    facts.callable(ctx, self.owner, Callable::Host(index))?
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

    /// Exposes an immutable view for the interprocedural solver.
    pub fn world(&self) -> World<'_> {
        World {
            program: &self.script.inner.code.program,
            source_owner: self.owner,
            contracts: &self.contracts.data,
            hosts: &self.hosts.data,
            globals: &self.globals.data,
        }
    }

    /// Analyzes a prepared entry only when its initialization and input scope are modeled.
    pub fn analyze(
        &self,
        ctx: &mut CallContext,
        facts: &mut Facts,
        function: usize,
        inputs: &[Input],
    ) -> Result<Analysis> {
        ctx.checkpoint()?;
        if !self.incomplete.data.is_empty() {
            let mut incomplete = Buffer::empty();
            incomplete.push(ctx, (function, 0))?;
            return Ok(Analysis {
                returns: Atom::Unknown.fact(),
                throws: u8::MAX,
                issues: Buffer::empty(),
                incomplete,
                contexts: 0,
            });
        }
        calls::analyze(ctx, facts, self.world(), function, inputs)
    }
}
